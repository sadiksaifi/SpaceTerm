"""Disposable source trust matches every generated-key dry-run reader."""

import json
import os
import re
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, core_package, release_dry_run, update_signing


class DryRunTrustTests(unittest.TestCase):
    def test_generated_key_is_compiled_trust_and_installer_trust_without_a_runtime_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "packaging").mkdir()
            shutil.copyfile(ROOT / "packaging/install.sh", root / "packaging/install.sh")
            seed = bytes(range(32))
            release_dry_run.configure_trust(seed, root)
            keys = update_signing.release_trust(root)
            self.assertEqual(keys[0], update_signing.derived_public_key(seed))
            self.assertNotEqual(keys[0], keys[1])
            source = (root / "packaging/install.sh").read_text()
            pinned = dict(
                re.findall(r"^readonly UPDATE_KEY_(CURRENT|BACKUP)='([^'\n]*)'$", source, re.M)
            )
            self.assertEqual(pinned, {"CURRENT": keys[0], "BACKUP": keys[1]})
            self.assertNotIn(update_signing.encoded_seed(seed), source)
            self.assertNotIn(
                update_signing.encoded_seed(seed), (root / update_signing.TRUST).read_text()
            )
            self.assertEqual(
                json.loads((root / update_signing.TRUST).read_bytes())["current"], keys[0]
            )
            archive, signature = root / "archive", root / "archive.sig"
            archive.write_bytes(b"compiled product bytes")
            update_signing.sign(seed, archive, signature)
            update_signing.verify(keys, archive, signature)
            # Native verifier jobs receive only the tracked public trust, never the seed.
            with (
                patch.object(core_package, "ROOT", root),
                patch.dict(
                    os.environ,
                    {update_signing.DRY_RUN: "1", "PATH": os.environ["PATH"]},
                    clear=True,
                ),
            ):
                update_signing.verify(core_package.artifact_trust("spaceterm"), archive, signature)
