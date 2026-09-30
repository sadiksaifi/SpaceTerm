#!/usr/bin/env python3
"""Generate bundle and packager metadata from the build's Git identity."""

import argparse
import base64
import json
import plistlib
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--sparkle", type=Path)
    args = parser.parse_args()
    stage = ROOT / "target/package-macos"
    config = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["metadata"]["packager"]
    # The staged Info.plist is the one source of the bundle's name and identifier.
    identity = plistlib.loads((stage / "Info.plist").read_bytes())
    config["product-name"] = identity["CFBundleName"]
    config["identifier"] = identity["CFBundleIdentifier"]
    config["binaries"] = [{"path": identity["CFBundleExecutable"], "main": True}]
    config["version"] = args.version
    config["binaries-dir"] = str(args.binaries)
    if args.sparkle:
        key = (ROOT / "packaging/macos/update-public-key.txt").read_text().strip()
        if len(base64.b64decode(key, validate=True)) != 32:
            raise SystemExit("A valid release public key is required")
        config["macos"]["frameworks"] = [str(args.sparkle / "Sparkle.framework")]
        entitlements = plistlib.loads((ROOT / "packaging/macos/Entitlements.plist").read_bytes())
        # Ad hoc binaries have no team identity with which Library Validation can trust Sparkle.
        entitlements["com.apple.security.cs.disable-library-validation"] = True
        (stage / "Entitlements.plist").write_bytes(plistlib.dumps(entitlements))
        config["macos"]["entitlements"] = str(stage / "Entitlements.plist")
        plist = plistlib.loads((stage / "Info.plist").read_bytes())
        plist.update({
            "SUFeedURL": "https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/appcast.xml",
            "SUPublicEDKey": key,
            "SUEnableAutomaticChecks": False,
            "SUAutomaticallyUpdate": False,
            "SUEnableSystemProfiling": False,
            "SURequireSignedFeed": True,
            "SUSignedFeedFailureExpirationInterval": 0,
            "SUVerifyUpdateBeforeExtraction": True,
        })
        (stage / "Info.plist").write_bytes(plistlib.dumps(plist))
    (stage / "packager.json").write_text(json.dumps(config))


if __name__ == "__main__":
    main()
