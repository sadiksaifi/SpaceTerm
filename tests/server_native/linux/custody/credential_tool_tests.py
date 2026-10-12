"""Run the actual patched ELF with test-only native peers, without host credentials."""

import argparse
import base64
import os
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

NAME = "--name=spaceterm.boot." + "0" * 64
COMMON = ["--no-ask-password", "--refuse-null", "--newline=no", NAME]
# Public upstream format identifiers and a deliberately unauthentic ciphertext.
HOST = bytes.fromhex("5a1c6a86df9d4096b1d5a65e0862f19a")
NULL = bytes.fromhex("058469daf6f54324800549da0f8ea2fb")
BLOB = base64.b64encode(HOST + struct.pack("<IIII", 32, 1, 12, 16) + bytes(84))


class CredentialToolTests(unittest.TestCase):
    root = Path("/")
    peer = Path("/")
    cases = 0

    def invoke(self, arguments, peer="ready", data=BLOB):
        type(self).cases += 1
        read, write = os.pipe()
        source = os.memfd_create("credential-ciphertext-peer", os.MFD_CLOEXEC)
        try:
            os.write(source, data)
            os.lseek(source, 0, os.SEEK_SET)
            environment = {
                "PATH": "/usr/bin:/bin",
                "LC_ALL": "C",
                "OPENSSL_CONF": str(
                    self.root / "share/spaceterm-server/credential-tools/openssl.cnf"
                ),
                "OPENSSL_MODULES": str(self.root / "lib/credential-tools"),
                "LD_PRELOAD": str(self.peer),
                "ST_CREDENTIAL_PEER": peer,
                "ST_CREDENTIAL_PEER_FD": str(write),
            }
            result = subprocess.run(
                [self.root / "libexec/credential-tools/systemd-creds", *arguments],
                env=environment,
                stdin=source,
                capture_output=True,
                pass_fds=(write,),
                timeout=10,
                check=False,
            )
            offset = os.lseek(source, 0, os.SEEK_CUR)
            os.close(write)
            write = -1
            events = os.read(read, 37)
            self.assertTrue(
                len(events) == 36,
                f"native peer observation incomplete: status={result.returncode}, bytes={len(events)}",
            )
            self.assertTrue(len(result.stdout) == 0, "unexpected credential-tool output")
            self.assertTrue(
                result.stderr in (b"", b"credential-tool-policy-unavailable\n"),
                "credential-tool diagnostics are not bounded classifications",
            )
            self.assertNotEqual(result.returncode, 0, "invalid input unexpectedly succeeded")
            return struct.unpack("=9I", events), offset
        finally:
            os.close(source)
            os.close(read)
            if write >= 0:
                os.close(write)

    def test_dumpable_and_lock_failures_precede_input_and_native_keys(self):
        for peer, expected in (("set", (1, 0, 0)), ("get", (1, 1, 0)), ("lock", (1, 1, 1))):
            with self.subTest(peer=peer):
                events, offset = self.invoke([*COMMON, "decrypt", "-", "-"], peer)
                self.assertEqual(events[:3], expected)
                self.assertEqual(events[4:8], (0, 0, 0, 0))
                self.assertEqual(offset, 0)

    def test_unsupported_interfaces_reject_before_input_and_keys(self):
        for extra in (
            "--allow-null",
            "--user",
            "--uid=0",
            "--tpm2-device=list",
            "--with-key=auto",
            "--tpm2-signature=/untrusted",
            "--refuse-null",
        ):
            with self.subTest(option=extra):
                events, offset = self.invoke([*COMMON, extra, "decrypt", "-", "-"])
                self.assertEqual(events[:3], (1, 1, 0))
                self.assertEqual(events[4:8], (0, 0, 0, 0))
                self.assertEqual(offset, 0)
        for operands in (
            ("setup",),
            ("decrypt", "/untrusted", "-"),
            ("encrypt", "-", "-"),
            ("cat", "untrusted"),
        ):
            with self.subTest(operation=operands[0]):
                events, offset = self.invoke([*COMMON, *operands])
                self.assertEqual(events[:3], (1, 1, 0))
                self.assertEqual(events[4:8], (0, 0, 0, 0))
                self.assertEqual(offset, 0)

    def test_null_unknown_and_scoped_ciphertexts_never_access_native_keys(self):
        for recipe in (NULL, bytes(16), bytes.fromhex("55b9ed1d38594d43a8319d2ebb332ac6")):
            with self.subTest(recipe=recipe.hex()):
                events, offset = self.invoke(
                    [*COMMON, "decrypt", "-", "-"], data=base64.b64encode(recipe + bytes(116))
                )
                self.assertEqual(events[:3], (1, 1, 1))
                self.assertEqual(events[4:8], (0, 0, 0, 0))
                self.assertGreater(offset, 0)

    def test_missing_host_storage_never_provisions_or_repairs(self):
        for peer, opened in (("directory-missing", 0), ("key-missing", 1)):
            with self.subTest(peer=peer):
                events, _ = self.invoke([*COMMON, "decrypt", "-", "-"], peer)
                self.assertEqual(events[:3], (1, 1, 1))
                self.assertEqual(events[4:8], (1, opened, 0, 0))

    def test_foreign_machine_key_is_preserved_and_read_buffer_is_wiped(self):
        events, _ = self.invoke([*COMMON, "decrypt", "-", "-"], "foreign")
        self.assertEqual(events[:3], (1, 1, 1))
        self.assertEqual(events[4:8], (1, 1, 0, 1))
        self.assertGreaterEqual(events[8], 2)  # Peer stack and upstream erase cleanup.

    def test_oversized_key_rejects_before_key_read(self):
        events, _ = self.invoke([*COMMON, "decrypt", "-", "-"], "oversized")
        self.assertEqual(events[4:8], (1, 1, 0, 0))

    def test_supported_host_recipe_exercises_real_crypto_failure_without_plaintext(self):
        events, _ = self.invoke([*COMMON, "decrypt", "-", "-"])
        self.assertEqual(events[:3], (1, 1, 1))
        self.assertEqual(events[4:8], (1, 1, 0, 1))
        self.assertGreaterEqual(events[8], 2)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--peer-source", required=True, type=Path)
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="credential-native-peers-") as temporary:
        peer = Path(temporary) / "peer.so"
        compilation = subprocess.run(
            [
                "gcc",
                "-shared",
                "-fPIC",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                arguments.peer_source,
                "-o",
                peer,
                "-ldl",
            ],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=30,
            check=False,
        )
        if compilation.returncode:
            raise SystemExit("credential-tool-native-peer-build-unavailable")
        CredentialToolTests.root = arguments.root
        CredentialToolTests.peer = peer
        result = unittest.TextTestRunner().run(
            unittest.defaultTestLoader.loadTestsFromTestCase(CredentialToolTests)
        )
        print(f"native-peer-cases={CredentialToolTests.cases}; privileged-evidence=unrun")
        raise SystemExit(0 if result.wasSuccessful() and result.testsRun == 7 else 1)
