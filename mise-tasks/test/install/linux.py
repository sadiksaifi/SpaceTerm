#!/usr/bin/env python
# MISE description="Run the release installer against a served fixture release on Linux"
"""Run install.sh against a served fixture release, real openssl and real desktop archives."""

import base64
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from pathlib import Path

from spaceterm_tasks.package_linux import stage, write_archive
from spaceterm_tasks.release import RELEASES
from tests.installer_fixture import (
    CURL,
    TOKEN,
    Release,
    build_document,
    core_archive,
    fake,
    render,
    run_with_tty,
)

APPLICATION_ID = "io.github.sadiksaifi.spaceterm"
PLATFORM = "linux-x86_64"
# A native executable keeps its own path in /proc/PID/exe, which the installer matches. It
# answers --build-info as the desktop does, so the installer can read the installed version.
DESKTOP = r"""
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--build-info") == 0) {
        printf("{\"schema\":1,\"identity\":\"spaceterm\",\"version\":\"%s\"}\n", VERSION);
        return 0;
    }
    for (;;) pause();
}
"""


def core_name(version):
    return f"spaceterm-core-{version}-{PLATFORM}.tar.gz"


def desktop_name(version):
    return f"spaceterm-desktop-{version}-{PLATFORM}.tar.gz"


@unittest.skipUnless(sys.platform == "linux", "the Linux installer runs only on Linux")
class InstallTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ("cc", "tic", "desktop-file-validate", "openssl"):
            assert shutil.which(tool), f"{tool} is required; run mise doctor project"
        cls.keys_directory = tempfile.TemporaryDirectory(prefix="spaceterm-install-keys-")
        keys = Path(cls.keys_directory.name)
        cls.keys = {}
        for name in ("current", "backup", "rogue"):
            private = keys / f"{name}.pem"
            subprocess.run(
                ["openssl", "genpkey", "-algorithm", "ed25519", "-out", private], check=True
            )
            public = subprocess.run(
                ["openssl", "pkey", "-in", private, "-pubout", "-outform", "DER"],
                check=True,
                capture_output=True,
            ).stdout[-32:]
            cls.keys[name] = (private, base64.b64encode(public).decode())

    @classmethod
    def tearDownClass(cls):
        cls.keys_directory.cleanup()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="spaceterm-install-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.home = self.root / "home"
        self.home.mkdir()
        # Brackets, quotes, and spaces are literal path characters, not matching or Exec syntax.
        self.install_dir = self.root / 'lib [personal] "quoted" $dollar'
        self.data_home = self.root / "data home"
        self.temporary = self.root / "tmp"
        self.temporary.mkdir()
        self.requests = self.root / "requests.log"
        self.core_log = self.root / "core.log"
        self.sudo_log = self.root / "sudo.log"
        self.release = Release(self.root)
        self.desktop_builds = {}

        # The network, sudo, loginctl, and the host's glibc, architecture, and Vulkan loader are
        # the controlled external boundaries. Unpacking, openssl, and process checks are real.
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake(self.bin, "curl", CURL)
        fake(self.bin, "getconf", '#!/bin/sh\necho "glibc ${FAKE_GLIBC:-2.35}"\n')
        fake(
            self.bin,
            "uname",
            '#!/bin/sh\nif [ "$1" = -m ]; then echo "${FAKE_MACHINE:-x86_64}"; '
            'else exec /usr/bin/env -i PATH=/usr/bin:/bin uname "$@"; fi\n',
        )
        fake(
            self.bin,
            "ldconfig",
            '#!/bin/sh\n[ -n "${FAKE_NO_VULKAN:-}" ] || '
            "printf '\\tlibvulkan.so.1 (libc6,x86-64) => /usr/lib/libvulkan.so.1\\n'\n",
        )
        fake(self.bin, "loginctl", '#!/bin/sh\necho "${FAKE_LINGER:-no}"\n')
        fake(
            self.bin,
            "sudo",
            '#!/bin/sh\nprintf \'%s\\n\' "$*" >>"$SUDO_LOG"\nexit "${FAKE_SUDO_STATUS:-0}"\n',
        )
        self.installer = render(
            self.root, current=self.keys["current"][1], backup=self.keys["backup"][1]
        )
        self.publish("1.2.0")

    # Fixtures.

    def sign(self, path, key="current", signature=None):
        signature = signature or Path(f"{path}.sig")
        subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-sign",
                "-inkey",
                self.keys[key][0],
                "-rawin",
                "-in",
                path,
                "-out",
                signature,
            ],
            check=True,
        )
        return signature

    def core(self, version, key="current", **options):
        directory = self.root / f"core-{version}-{key}-{len(options)}"
        directory.mkdir(exist_ok=True)
        archive = core_archive(directory / core_name(version), version, **options)
        return archive, self.sign(archive, key)

    def desktop(self, version, key="current"):
        directory = self.root / f"desktop-{version}-{key}"
        archive = directory / desktop_name(version)
        if not archive.exists():
            executable = self.root / f"spaceterm-desktop-{version}"
            subprocess.run(
                ["cc", "-x", "c", f'-DVERSION="{version}"', "-", "-o", executable],
                input=DESKTOP,
                text=True,
                check=True,
                capture_output=True,
            )
            stage("spaceterm", executable, directory / "spaceterm")
            write_archive(directory / "spaceterm", archive)
            self.sign(archive, key)
        return archive, Path(f"{archive}.sig")

    def publish(self, version, latest=True, desktop=False, core_key="current", **options):
        core, core_signature = self.core(version, core_key, **options)
        assets = {core.name: core, core_signature.name: core_signature}
        if desktop:
            archive, signature = self.desktop(version)
            assets.update({archive.name: archive, signature.name: signature})
        self.release.publish(version, assets, latest=latest)

    # Running the installer.

    def environment(self, **extra):
        return {
            "HOME": str(self.home),
            "XDG_DATA_HOME": str(self.data_home),
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "SHELL": "/bin/bash",
            "SERVED": json.dumps(self.release.served),
            "REQUESTS": str(self.requests),
            "CORE_LOG": str(self.core_log),
            "CORE_RELATIVE": "bin/spaceterm",
            "SUDO_LOG": str(self.sudo_log),
            "SPACETERM_INSTALL_DIR": str(self.install_dir),
            "TMPDIR": str(self.temporary),
            **extra,
        }

    def install(self, *arguments, installer=None, **extra):
        """Run without a controlling terminal, as over ssh without a terminal or in CI."""
        return subprocess.run(
            ["sh", str(installer or self.installer), *arguments],
            env=self.environment(**extra),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=120,
            start_new_session=True,
        )

    def install_with_tty(self, *arguments, answers="", **extra):
        return run_with_tty(
            ["sh", str(self.installer), *arguments], self.environment(**extra), answers
        )

    def succeed(self, *arguments, **extra):
        result = self.install(*arguments, **extra)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    # Observations.

    def requested(self):
        return self.requests.read_text().splitlines() if self.requests.exists() else []

    def core_calls(self):
        return self.core_log.read_text().splitlines() if self.core_log.exists() else []

    def sudo_calls(self):
        return self.sudo_log.read_text().splitlines() if self.sudo_log.exists() else []

    def installations(self):
        directory = self.home / ".local/share/spaceterm/core/installations"
        if not directory.exists():
            return {}
        return {
            path.name: json.loads((path / "build.json").read_text())["version"]
            for path in directory.iterdir()
        }

    def launcher(self):
        return self.home / ".local/bin/spaceterm"

    def desktop_target(self):
        return self.install_dir / "spaceterm"

    def desktop_version(self):
        output = subprocess.run(
            [self.desktop_target() / "bin/spaceterm-desktop", "--build-info"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        return json.loads(output)["version"]

    def assert_no_leftovers(self):
        self.assertEqual(list(self.temporary.iterdir()), [])

    def assert_nothing_ran(self):
        self.assertEqual(self.core_calls(), [])
        self.assertEqual(self.installations(), {})
        self.assertFalse(os.path.lexists(self.launcher()))
        self.assert_no_leftovers()

    def run_desktop(self):
        process = subprocess.Popen([str(self.home / ".local/bin/spaceterm-desktop")])
        self.addCleanup(process.wait)
        self.addCleanup(process.kill)
        executable = str(self.desktop_target() / "bin/spaceterm-desktop")
        deadline = time.monotonic() + 5
        while os.readlink(f"/proc/{process.pid}/exe") != executable:
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)

    # SpaceTerm Core.

    def test_default_installs_only_core_after_verifying_its_signature(self):
        result = self.succeed()
        self.assertEqual(
            self.requested(),
            [
                f"{RELEASES}/latest/download/SHA256SUMS",
                f"{RELEASES}/download/v1.2.0/{core_name('1.2.0')}",
                f"{RELEASES}/download/v1.2.0/{core_name('1.2.0')}.sig",
            ],
        )
        [call] = self.core_calls()
        self.assertRegex(call, r"^internal retain .*/runtime/core$")
        self.assertEqual(list(self.installations().values()), ["1.2.0"])
        self.assertTrue(self.launcher().is_symlink())
        self.assertFalse(self.desktop_target().exists())
        self.assertIn("Run spaceterm setup to start SpaceTerm Core.", result.stdout)
        self.assert_no_leftovers()

    def test_version_installs_the_requested_release(self):
        self.publish("1.1.0", latest=False)
        self.succeed("--version", "1.1.0")
        self.assertEqual(
            self.requested(),
            [
                f"{RELEASES}/download/v1.1.0/SHA256SUMS",
                f"{RELEASES}/download/v1.1.0/{core_name('1.1.0')}",
                f"{RELEASES}/download/v1.1.0/{core_name('1.1.0')}.sig",
            ],
        )
        self.assertEqual(list(self.installations().values()), ["1.1.0"])

    def test_a_new_version_installs_beside_the_existing_core(self):
        self.publish("1.1.0", latest=False)
        self.succeed("--version", "1.1.0")
        [first] = self.installations()
        launcher = os.readlink(self.launcher())
        result = self.succeed()
        self.assertEqual(sorted(self.installations().values()), ["1.1.0", "1.2.0"])
        self.assertIn(first, self.installations())
        # The existing installation keeps its launcher; SpaceTerm Core moves it on restart.
        self.assertEqual(os.readlink(self.launcher()), launcher)
        self.assertIn(
            "SpaceTerm Core 1.2.0 is installed beside SpaceTerm Core 1.1.0. "
            "A running SpaceTerm Core keeps its version until it restarts.",
            result.stdout,
        )

    def test_the_installed_version_is_not_downloaded_again(self):
        self.succeed()
        self.requests.unlink()
        result = self.succeed()
        self.assertIn("SpaceTerm Core 1.2.0 is already installed", result.stdout)
        self.assertEqual(self.requested(), [f"{RELEASES}/latest/download/SHA256SUMS"])
        self.assertEqual(len(self.core_calls()), 1)

    def test_downgrades_are_refused_before_any_archive_downloads(self):
        self.succeed()
        self.requests.unlink()
        self.publish("1.1.0", latest=False)
        for arguments in (("--version", "1.1.0"), ("--version", "1.1.0", "--desktop")):
            with self.subTest(arguments=arguments):
                result = self.install(*arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(
                    "SpaceTerm Core 1.2.0 is installed, which is newer than 1.1.0; "
                    "the installer never downgrades",
                    result.stderr,
                )
                self.assertEqual(self.requested(), [f"{RELEASES}/download/v1.1.0/SHA256SUMS"])
                self.requests.unlink()
        self.assertEqual(list(self.installations().values()), ["1.2.0"])

    def test_a_newer_desktop_app_is_never_downgraded(self):
        self.publish("1.3.0", desktop=True)
        self.succeed("--desktop")
        # Only the desktop app is newer than the release.
        shutil.rmtree(self.home / ".local/share/spaceterm")
        self.requests.unlink()
        self.publish("1.2.0", desktop=True)
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SpaceTerm 1.3.0 is installed, which is newer than 1.2.0", result.stderr)
        self.assertEqual(self.requested(), [f"{RELEASES}/latest/download/SHA256SUMS"])
        self.assertEqual(self.desktop_version(), "1.3.0")

    def test_signature_failures_stop_before_anything_runs(self):
        archive, _ = self.core("1.2.0")
        truncated = self.root / "truncated.sig"
        truncated.write_bytes(self.sign(archive).read_bytes()[:63])
        other = self.root / "other"
        other.write_bytes(b"other bytes")
        for label, signature, message in (
            ("unknown key", self.core("1.2.0", "rogue")[1], "does not carry a valid SpaceTerm"),
            ("other bytes", self.sign(other), "does not carry a valid SpaceTerm"),
            ("truncated", truncated, "has a malformed signature"),
        ):
            with self.subTest(label):
                self.release.publish(
                    "1.2.0", {archive.name: archive, f"{archive.name}.sig": signature}
                )
                result = self.install()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assert_nothing_ran()

    def test_the_backup_key_is_trusted(self):
        self.publish("1.2.0", core_key="backup")
        self.succeed()
        self.assertEqual(list(self.installations().values()), ["1.2.0"])

    def test_a_checksum_mismatch_stops_before_anything_runs(self):
        archive, signature = self.core("1.2.0")
        self.release.publish(
            "1.2.0",
            {archive.name: archive, signature.name: signature},
            sums=f"{'f' * 64}  {archive.name}\n{'0' * 64}  {signature.name}\n",
        )
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match its published checksum", result.stderr)
        self.assert_nothing_ran()

    def test_signed_archives_with_links_or_parent_paths_are_refused_unextracted(self):
        for member in (
            ("core/bin/link", b"", 0o755, tarfile.SYMTYPE, "/bin/sh"),
            ("core/bin/hard", b"", 0o755, tarfile.LNKTYPE, "core/bin/spaceterm"),
            ("../escaped", b"x"),
            ("core/../../escaped", b"x"),
        ):
            with self.subTest(member=member[0], kind=member[3] if len(member) > 3 else "file"):
                self.publish("1.2.0", extra=[member])
                result = self.install()
                self.assertNotEqual(result.returncode, 0)
                self.assertRegex(result.stderr, "invalid layout|only files and directories")
                self.assertFalse((self.root / "escaped").exists())
                self.assert_nothing_ran()

    def test_an_archive_for_another_version_or_identity_is_refused_before_it_runs(self):
        for document in (build_document("1.1.0"), build_document("1.2.0", "spaceterm-preflight")):
            with self.subTest(document=document):
                self.publish("1.2.0", document=document)
                result = self.install()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("does not hold the SpaceTerm Core 1.2.0 release", result.stderr)
                self.assert_nothing_ran()

    def test_a_dry_run_accepts_placeholder_keys_and_says_so(self):
        installer = render(self.root, kind="dry-run", team_id="")
        result = self.install(installer=installer)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("comes from a dry run", result.stderr)
        self.assertIn("skipped the Ed25519 check", result.stderr)
        self.assertEqual(list(self.installations().values()), ["1.2.0"])

    def test_placeholder_keys_refuse_a_release_before_downloading(self):
        installer = render(self.root)
        result = self.install(installer=installer)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("placeholder update keys", result.stderr)
        self.assertEqual(self.requested(), [])

    def test_unsupported_hosts_are_refused_before_downloading(self):
        for extra, message in (
            ({"FAKE_MACHINE": "aarch64"}, "requires an x86_64 computer"),
            ({"FAKE_GLIBC": "2.34"}, "requires glibc 2.35 or newer"),
            # musl has no GNU_LIBC_VERSION.
            ({"FAKE_GLIBC": ""}, "requires glibc 2.35 or newer"),
        ):
            with self.subTest(extra=extra):
                if extra.get("FAKE_GLIBC") == "":
                    fake(self.bin, "getconf", "#!/bin/sh\nexit 1\n")
                result = self.install(**extra)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.requested(), [])

    # The desktop app.

    def test_desktop_installs_the_verified_desktop_app_and_core(self):
        self.publish("1.2.0", desktop=True)
        self.succeed("--desktop")
        self.assertEqual(
            self.requested()[-2:],
            [
                f"{RELEASES}/download/v1.2.0/{desktop_name('1.2.0')}",
                f"{RELEASES}/download/v1.2.0/{desktop_name('1.2.0')}.sig",
            ],
        )
        self.assertEqual(list(self.installations().values()), ["1.2.0"])
        self.assertEqual(self.desktop_version(), "1.2.0")
        launcher = self.home / ".local/bin/spaceterm-desktop"
        self.assertEqual(
            os.readlink(launcher), str(self.desktop_target() / "bin/spaceterm-desktop")
        )
        icon = self.data_home / f"icons/hicolor/scalable/apps/{APPLICATION_ID}.svg"
        self.assertTrue(icon.is_symlink() and icon.is_file())
        entry = self.data_home / f"applications/{APPLICATION_ID}.desktop"
        subprocess.run(["desktop-file-validate", str(entry)], check=True)
        executable = str(self.desktop_target() / "bin/spaceterm-desktop").replace("$", "\\\\$")
        executable = executable.replace('"', '\\\\"')
        self.assertIn(f'Exec="{executable}" %u\n', entry.read_text())
        self.assertEqual(sorted(path.name for path in self.install_dir.iterdir()), ["spaceterm"])
        self.assert_no_leftovers()

    def test_a_desktop_archive_with_a_bad_signature_is_not_installed(self):
        core, core_signature = self.core("1.2.0")
        archive, _ = self.desktop("1.2.0")
        rogue = self.sign(archive, "rogue", self.root / "rogue.sig")
        self.release.publish(
            "1.2.0",
            {
                core.name: core,
                core_signature.name: core_signature,
                archive.name: archive,
                f"{archive.name}.sig": rogue,
            },
        )
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"{archive.name} does not carry a valid SpaceTerm signature", result.stderr)
        self.assertFalse(self.install_dir.exists())
        self.assert_no_leftovers()

    def test_a_running_desktop_app_is_refused_before_downloading(self):
        self.publish("1.1.0", latest=False, desktop=True)
        self.succeed("--desktop", "--version", "1.1.0")
        self.publish("1.2.0", desktop=True)
        self.requests.unlink()
        self.run_desktop()
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.requested(), [f"{RELEASES}/latest/download/SHA256SUMS"])
        self.assertEqual(list(self.installations().values()), ["1.1.0"])
        self.assertEqual(self.desktop_version(), "1.1.0")

    def test_a_failed_replacement_restores_the_existing_desktop_app(self):
        self.publish("1.1.0", latest=False, desktop=True)
        self.succeed("--desktop", "--version", "1.1.0")
        self.publish("1.2.0", desktop=True)
        # Fail only the rename that moves the unpacked release into place.
        fake(
            self.bin,
            "mv",
            '#!/bin/sh\ncase "$1" in\n    */.spaceterm.install.*/new) exit 1 ;;\nesac\n'
            'exec /bin/mv "$@"\n',
        )
        result = self.install("--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not install SpaceTerm", result.stderr)
        self.assertEqual(self.desktop_version(), "1.1.0")
        self.assertEqual(sorted(path.name for path in self.install_dir.iterdir()), ["spaceterm"])
        self.assert_no_leftovers()

    def test_the_desktop_installs_without_a_vulkan_loader_with_a_warning(self):
        self.publish("1.2.0", desktop=True)
        result = self.succeed("--desktop", FAKE_NO_VULKAN="1")
        self.assertIn("libvulkan.so.1", result.stderr)
        self.assertEqual(self.desktop_version(), "1.2.0")

    def test_a_path_desktop_launchers_cannot_run_is_refused(self):
        install_dir = self.home / "percent%path"
        result = self.install("--desktop", SPACETERM_INSTALL_DIR=str(install_dir))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("desktop launchers cannot run", result.stderr)
        self.assertFalse(install_dir.exists())
        self.assertEqual(self.requested(), [])

    # Linger.

    def test_without_a_terminal_linger_is_skipped_and_the_installer_says_so(self):
        result = self.succeed()
        self.assertIn(
            "No terminal is attached, so the installer did not ask to keep SpaceTerm Core "
            "running after you log out. Run the installer again with --linger",
            result.stdout,
        )
        self.assertEqual(self.sudo_calls(), [])

    def test_linger_enables_linger_without_asking(self):
        result = self.succeed("--linger")
        self.assertEqual(self.sudo_calls(), [f"-n loginctl enable-linger {os.getuid()}"])
        self.assertNotIn("No terminal is attached", result.stdout)

    def test_linger_that_sudo_refuses_fails_the_installation(self):
        result = self.install("--linger", FAKE_SUDO_STATUS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sudo needs a terminal to ask for your password", result.stderr)

    def test_an_account_that_already_lingers_is_left_alone(self):
        result = self.succeed("--linger", FAKE_LINGER="yes")
        self.assertEqual(self.sudo_calls(), [])
        self.assertNotIn("log out", result.stdout)

    def test_the_linger_prompt_reads_the_terminal(self):
        for answer, calls in (("y\n", [f"loginctl enable-linger {os.getuid()}"]), ("n\n", [])):
            with self.subTest(answer=answer):
                self.sudo_log.unlink(missing_ok=True)
                result, terminal = self.install_with_tty(answers=answer)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("Keep SpaceTerm Core running after you log out?", terminal)
                self.assertEqual(self.sudo_calls(), calls)
                if not calls:
                    self.assertIn("SpaceTerm Core stops when you log out.", result.stdout)

    # Shell profiles.

    def test_profile_edits_add_local_bin_once_for_the_login_shell(self):
        line = 'export PATH="$HOME/.local/bin:$PATH"'
        fish = "fish_add_path --path $HOME/.local/bin"
        zdotdir = self.root / "zdotdir"
        for shell, extra, profile, expected in (
            ("/bin/bash", {}, self.home / ".bashrc", line),
            ("/usr/bin/zsh", {}, self.home / ".zshrc", line),
            ("/usr/bin/zsh", {"ZDOTDIR": str(zdotdir)}, zdotdir / ".zshrc", line),
            ("/usr/bin/fish", {}, self.home / ".config/fish/conf.d/spaceterm.fish", fish),
        ):
            with self.subTest(shell=shell, profile=profile):
                profile.parent.mkdir(parents=True, exist_ok=True)
                if profile.name != "spaceterm.fish":
                    profile.write_text("# existing\n")
                for _ in range(2):
                    result = self.succeed(SHELL=shell, **extra)
                text = profile.read_text()
                self.assertEqual(text.count(expected), 1)
                self.assertTrue(text.endswith(f"# Added by the SpaceTerm installer\n{expected}\n"))
                if profile.name != "spaceterm.fish":
                    self.assertTrue(text.startswith("# existing\n\n"))
                self.assertNotIn("Add ", result.stdout)
                profile.unlink()

    def test_the_profile_is_left_alone_when_asked_or_not_needed(self):
        bin_dir = self.home / ".local/bin"
        for arguments, extra, advice in (
            (("--no-modify-path",), {}, True),
            ((), {"SHELL": "/bin/dash"}, True),
            ((), {"PATH": f"{self.bin}:{bin_dir}:/usr/bin:/bin"}, False),
        ):
            with self.subTest(arguments=arguments, extra=extra):
                result = self.succeed(*arguments, **extra)
                self.assertEqual(
                    sorted(path.name for path in self.home.iterdir() if path.is_file()), []
                )
                self.assertFalse((self.home / ".config").exists())
                self.assertEqual(
                    f"Add {bin_dir} to PATH to run spaceterm from a terminal." in result.stdout,
                    advice,
                )

    # Uninstall.

    def test_uninstall_without_a_terminal_previews_and_names_the_confirmation(self):
        self.succeed()
        result = self.install("--uninstall")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Uninstall SpaceTerm Core from this machine?", result.stdout)
        self.assertIn(f"run: spaceterm uninstall --confirm {TOKEN}", result.stderr)
        self.assertEqual(self.core_calls()[1:], ["uninstall"])
        self.assertTrue(self.launcher().is_symlink())

    def test_uninstall_confirms_on_the_terminal(self):
        for answer, calls, code in (
            ("y\n", ["uninstall", f"uninstall --confirm {TOKEN}"], 0),
            ("n\n", ["uninstall"], 1),
        ):
            with self.subTest(answer=answer):
                shutil.rmtree(self.home / ".local", ignore_errors=True)
                self.succeed()
                self.core_log.unlink()
                result, terminal = self.install_with_tty("--uninstall", answers=answer)
                self.assertEqual(result.returncode, code, result.stdout + result.stderr)
                self.assertIn("Uninstall SpaceTerm Core?", terminal)
                # --delete-data is never passed: the installer keeps data.
                self.assertEqual(self.core_calls(), calls)
                self.assertEqual(self.launcher().is_symlink(), code != 0)

    def test_uninstall_without_core_reports_it_is_already_uninstalled(self):
        result = self.succeed("--uninstall")
        self.assertIn("SpaceTerm Core is already uninstalled", result.stdout)

    def test_uninstall_leaves_unmanaged_launchers_untouched(self):
        launcher = self.launcher()
        launcher.parent.mkdir(parents=True)
        for link in (False, True):
            with self.subTest(link=link):
                if link:
                    launcher.symlink_to("/another/tool")
                else:
                    launcher.write_text("local tool")
                result = self.succeed("--uninstall")
                self.assertIn("already uninstalled", result.stdout)
                if link:
                    self.assertEqual(launcher.readlink(), Path("/another/tool"))
                else:
                    self.assertEqual(launcher.read_text(), "local tool")
                launcher.unlink()

    def test_uninstall_desktop_removes_the_desktop_app_and_keeps_user_data(self):
        self.publish("1.2.0", desktop=True)
        self.succeed("--desktop")
        settings = self.data_home / "spaceterm/settings.json"
        settings.parent.mkdir(parents=True)
        settings.write_text("{}")
        (self.install_dir / ".spaceterm.update").mkdir()
        result, _ = self.install_with_tty("--uninstall", "--desktop", answers="y\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.core_calls()[-1], f"uninstall --confirm {TOKEN}")
        self.assertEqual(list(self.install_dir.iterdir()), [])
        self.assertFalse(os.path.lexists(self.home / ".local/bin/spaceterm-desktop"))
        self.assertFalse((self.data_home / f"applications/{APPLICATION_ID}.desktop").exists())
        icon = self.data_home / f"icons/hicolor/scalable/apps/{APPLICATION_ID}.svg"
        self.assertFalse(os.path.lexists(icon))
        self.assertEqual(settings.read_text(), "{}")

    def test_uninstall_desktop_refuses_while_the_desktop_app_runs(self):
        self.publish("1.2.0", desktop=True)
        self.succeed("--desktop")
        self.core_log.unlink()
        self.run_desktop()
        result = self.install("--uninstall", "--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.core_calls(), [])
        self.assertEqual(self.desktop_version(), "1.2.0")

    def test_uninstall_desktop_refuses_while_an_updated_desktop_awaits_relaunch(self):
        self.publish("1.2.0", desktop=True)
        self.succeed("--desktop")
        self.run_desktop()
        # The updater exchanged the running installation into its staging directory.
        staging = self.install_dir / ".spaceterm.update"
        staging.mkdir()
        self.desktop_target().rename(staging / "tree")
        shutil.copytree(staging / "tree", self.desktop_target(), symlinks=True)
        result = self.install("--uninstall", "--desktop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertTrue((staging / "tree/bin/spaceterm-desktop").is_file())


if __name__ == "__main__":
    unittest.main()
