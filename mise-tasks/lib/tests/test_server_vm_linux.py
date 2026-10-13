"""Bounded native VM tools, authenticated images, and same-account SSH transport."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks import server_vm_linux as native


class JammySeedTests(unittest.TestCase):
    def test_seed_uses_one_account_and_no_custody_or_encrypted_disk_policy(self):
        data = native.jammy_seed(
            "a" * 64,
            b"ssh-ed25519 " + b"a" * 68,
            b"ssh-ed25519 " + b"b" * 68,
            b"host-private-fixture",
        )
        self.assertTrue(b'"name":"stvm"' in data)
        self.assertNotIn(b"stvm-server", data)
        self.assertNotIn(b"stvm-admin", data)
        self.assertNotIn(b"systemd-creds", data)
        self.assertIn(b"CIDATA", data)


class NativeToolTests(unittest.TestCase):
    def test_tool_outputs_are_bounded_and_native_errors_are_not_relayed(self):
        for script, timeout in (
            ("print('x' * 70000)", 3),
            ("import time; time.sleep(30)", 0.1),
            ("import sys; sys.stderr.write('private detail'); sys.exit(7)", 3),
        ):
            with self.subTest(script=script), self.assertRaises(TaskError) as error:
                native.run_tool([sys.executable, "-I", "-c", script], timeout=timeout)
            self.assertNotIn("private detail", str(error.exception))

    def test_closed_output_is_not_permission_to_kill_a_still_finishing_tool(self):
        output = native.run_tool(
            [sys.executable, "-I", "-c", "import os,time; os.close(1); os.close(2); time.sleep(.1)"]
        )
        self.assertEqual(output, b"")

    def test_consuming_reap_interruption_never_reopens_the_group_signal_lease(self):
        real_wait = subprocess.Popen.wait
        consumed = []
        signals = []
        real_killpg = os.killpg
        first = KeyboardInterrupt()

        def consume_then_interrupt(process, timeout=None):
            result = real_wait(process, timeout=timeout)
            if not consumed:
                consumed.append(process.pid)
                raise first
            return result

        def signal_group(pid, signum):
            self.assertNotIn(pid, consumed, "consuming reap closed the signal lease")
            signals.append((pid, signum))
            return real_killpg(pid, signum)

        with (
            patch.object(subprocess.Popen, "wait", consume_then_interrupt),
            patch.object(os, "killpg", signal_group),
            self.assertRaises(KeyboardInterrupt) as error,
        ):
            native.run_tool([sys.executable, "-I", "-c", "print('done')"])
        self.assertIs(error.exception, first)
        self.assertEqual(len(consumed), 1)
        self.assertTrue(signals)

    def test_first_interrupt_survives_a_later_consuming_reap_interrupt(self):
        first, later = KeyboardInterrupt(), SystemExit(74)
        real_wait = subprocess.Popen.wait
        consumed = []
        real_read = os.read

        def read(fd, count):
            if count == 8192:
                raise first
            return real_read(fd, count)

        def wait(process, timeout=None):
            result = real_wait(process, timeout=timeout)
            if not consumed:
                consumed.append(process.pid)
                raise later
            return result

        with (
            patch.object(os, "read", read),
            patch.object(subprocess.Popen, "wait", wait),
            self.assertRaises(KeyboardInterrupt) as error,
        ):
            native.run_tool([sys.executable, "-I", "-c", "print('done')"])
        self.assertIs(error.exception, first)
        self.assertEqual(len(consumed), 1)

    def test_environment_contains_no_signing_or_graphical_values(self):
        with patch.dict(os.environ, {"UPDATE_SIGNING_KEY": "secret", "DISPLAY": "private"}):
            output = native.run_tool(
                [sys.executable, "-I", "-c", "import os,json; print(json.dumps(dict(os.environ)))"]
            )
        environment = json.loads(output)
        self.assertEqual(environment["PATH"], "/usr/bin:/bin")
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertNotIn("UPDATE_SIGNING_KEY", environment)
        self.assertNotIn("DISPLAY", environment)


class ImageSignatureTests(unittest.TestCase):
    def test_signer_status_requires_exact_primary_and_rejects_bad_or_expired_signatures(self):
        fingerprint = "A" * 40
        valid = f"[GNUPG:] VALIDSIG {'B' * 40} 2026-10-13 1 0 4 0 22 8 00 {fingerprint}\n".encode()
        native.validate_signer(valid, fingerprint)
        for status in (
            valid.replace(fingerprint.encode(), b"C" * 40),
            valid + valid,
            valid + b"[GNUPG:] EXPKEYSIG 1 private\n",
            b"[GNUPG:] GOODSIG 1 private\n",
        ):
            with self.subTest(status=status), self.assertRaises(TaskError):
                native.validate_signer(status, fingerprint)

    def test_checksum_authentication_precedes_interpretation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            files = [root / name for name in ("checksums", "signature", "keyring")]
            for path, data in zip(files, (b"malformed checksum", b"sig", b"keyring"), strict=True):
                path.write_bytes(data)
                path.chmod(0o600)
            with patch.object(
                native, "run_tool", side_effect=TaskError("verification-failed")
            ) as run:
                with self.assertRaisesRegex(TaskError, "verification-failed"):
                    native.verify_checksums(
                        files[0],
                        files[1],
                        files[2],
                        "A" * 40,
                        hashlib.sha256(b"keyring").hexdigest(),
                    )
                self.assertEqual(run.call_count, 1)


class GuestTransportTests(unittest.TestCase):
    def test_transport_keeps_host_key_checks_and_never_runs_spaceterm_as_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("id_ed25519", "known_hosts"):
                (root / name).write_bytes(b"fixture")
                (root / name).chmod(0o600)
            with patch.object(native, "run_tool", return_value=b"{}") as run:
                native.guest_command(root, 25022, "status")
            command = run.call_args.args[0]
            self.assertIn("StrictHostKeyChecking=yes", command)
            self.assertIn("IdentityAgent=none", command)
            self.assertIn("ForwardAgent=no", command)
            self.assertEqual(command[-1], "spaceterm-server --json status")
            self.assertNotIn("sudo", command[-1])
            self.assertIn("stvm@127.0.0.1", command)
        with self.assertRaises(TaskError):
            native.guest_command(root, 25022, "arbitrary shell command")


class NativeProvisioningTests(unittest.TestCase):
    def test_oversized_or_external_image_is_rejected_before_conversion(self):
        from spaceterm_tasks.server_vm import Resources

        for info in (
            {"format": "qcow2", "virtual-size": 25 * 1024**3},
            {"format": "qcow2", "virtual-size": 1024, "backing-filename": "/foreign"},
        ):
            calls = []

            def tool(command, **kwargs):
                calls.append(command)
                if command[1] != "info":
                    self.fail("unadmitted conversion ran before source disk bounds")
                return json.dumps(info).encode()

            with (
                tempfile.TemporaryDirectory() as temporary,
                patch.object(native, "run_tool", side_effect=tool),
                self.assertRaises(TaskError),
            ):
                native.NativeVm().prepare(
                    Path(temporary), Path(temporary) / "source.qcow2", Resources(), "a" * 64
                )
            self.assertEqual(len(calls), 1)
