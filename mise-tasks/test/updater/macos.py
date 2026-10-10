#!/usr/bin/env python
# MISE description="Exercise authenticated updates and certificate identity transitions in isolated bundles"
"""Exercise Sparkle signature rejection, cancellation, installation and relaunch in temp apps."""

import base64
import functools
import http.server
import os
import plistlib
import shutil
import subprocess
import tempfile
import threading
import time
from contextlib import ExitStack
from pathlib import Path

from spaceterm_tasks import ROOT
from spaceterm_tasks import sparkle as SPARKLE
from spaceterm_tasks.macos_signing import release_keychain, verify_release_signature
from spaceterm_tasks.release import verify_feed
from tests.macos_signing_fixture import certificate


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, **kwargs).stdout


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass


def main():
    sparkle = SPARKLE.directory()
    with (
        tempfile.TemporaryDirectory(prefix="spaceterm-updater-test-") as temporary,
        ExitStack() as signing,
    ):
        root = Path(temporary)
        fingerprint, credentials = certificate(root / "signing")
        signing.enter_context(release_keychain(fingerprint, {**os.environ, **credentials}))
        server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0), functools.partial(QuietHandler, directory=root)
        )
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            seed = os.urandom(32)
            key = base64.b64encode(seed)
            public = run(
                "openssl",
                "pkey",
                "-inform",
                "DER",
                "-pubout",
                "-outform",
                "DER",
                input=bytes.fromhex("302e020100300506032b657004220420") + seed,
            )[-32:]
            entitlements = root / "Entitlements.plist"
            entitlements.write_bytes(
                plistlib.dumps(
                    {
                        "com.apple.security.device.audio-input": True,
                        "com.apple.security.cs.disable-library-validation": True,
                    }
                )
            )
            binary = root / "fixture"
            run(
                "xcrun",
                "clang",
                "-fobjc-arc",
                "-fblocks",
                "-mmacosx-version-min=26.0",
                "-F",
                str(sparkle),
                "-framework",
                "Sparkle",
                "-framework",
                "AppKit",
                "-framework",
                "Foundation",
                "-Wl,-rpath,@executable_path/../Frameworks",
                str(ROOT / "tests/macos_updater_fixture.m"),
                str(ROOT / "src/platform/macos_updater.m"),
                "-o",
                str(binary),
            )
            for scenario, mode, signed_old, signed_new in (
                ("tampered", "tampered", False, False),
                ("cancel", "cancel", False, False),
                ("install", "install", False, False),
                ("quit", "quit", False, False),
                ("certificate-install", "install", True, True),
                ("identity-transition", "install", False, True),
            ):
                directory = root / scenario
                directory.mkdir()
                log = directory / "events"
                log.touch()
                feed_url = f"http://127.0.0.1:{server.server_port}/{scenario}/appcast.xml"
                apps = []
                for version, name in (("0.1.0", "Installed"), ("0.1.1", "New")):
                    app = directory / name / "SpaceTermUpdateTest.app"
                    contents = app / "Contents"
                    (contents / "MacOS").mkdir(parents=True)
                    (contents / "Frameworks").mkdir()
                    shutil.copy2(binary, contents / "MacOS/Fixture")
                    run(
                        "ditto",
                        str(sparkle / "Sparkle.framework"),
                        str(contents / "Frameworks/Sparkle.framework"),
                    )
                    plist = {
                        "CFBundleIdentifier": f"io.github.sadiksaifi.spaceterm.updater-test.{scenario}",
                        "CFBundleName": "SpaceTermUpdateTest",
                        "CFBundleExecutable": "Fixture",
                        "CFBundlePackageType": "APPL",
                        "CFBundleVersion": version,
                        "CFBundleShortVersionString": version,
                        "LSMinimumSystemVersion": "26.0",
                        "LSUIElement": True,
                        "SUFeedURL": feed_url,
                        "SUPublicEDKey": base64.b64encode(public).decode(),
                        "SUEnableAutomaticChecks": False,
                        "SUAutomaticallyUpdate": False,
                        "SURequireSignedFeed": True,
                        "SUVerifyUpdateBeforeExtraction": True,
                        "NSAppTransportSecurity": {"NSAllowsLocalNetworking": True},
                        "SPTTestLog": str(log),
                        "SPTTestMode": mode,
                    }
                    (contents / "Info.plist").write_bytes(plistlib.dumps(plist))
                    signed = signed_old if version == "0.1.0" else signed_new
                    run(
                        "codesign",
                        "--force",
                        "--deep",
                        "--sign",
                        fingerprint if signed else "-",
                        "--timestamp=none",
                        "--options",
                        "runtime",
                        "--entitlements",
                        str(entitlements),
                        str(app),
                    )
                    if signed:
                        verify_release_signature(
                            app, fingerprint, plist["CFBundleIdentifier"], scenario
                        )
                    apps.append(app)
                archive = directory / "SpaceTerm-0.1.1-darwin-arm64.dmg"
                run(
                    "hdiutil",
                    "create",
                    "-quiet",
                    "-format",
                    "UDZO",
                    "-srcfolder",
                    str(apps[1].parent),
                    str(archive),
                )
                prefix = "https://github.com/sadiksaifi/SpaceTerm/releases/download/v0.1.1/"
                run(
                    str(sparkle / "bin/generate_appcast"),
                    "--ed-key-file",
                    "-",
                    "--maximum-deltas",
                    "0",
                    "--maximum-versions",
                    "1",
                    "--download-url-prefix",
                    prefix,
                    "--link",
                    "https://github.com/sadiksaifi/SpaceTerm/releases/tag/v0.1.1",
                    str(directory),
                    input=key,
                )
                feed = directory / "appcast.xml"
                signature = verify_feed(feed, archive, "0.1.1")
                run(
                    str(sparkle / "bin/sign_update"),
                    "--ed-key-file",
                    "-",
                    "--verify",
                    str(archive),
                    signature,
                    input=key,
                )
                run(
                    str(sparkle / "bin/sign_update"),
                    "--ed-key-file",
                    "-",
                    "--verify",
                    str(feed),
                    input=key,
                )
                # Exercise the production generator and verifier, then serve the fixture locally.
                feed.write_text(
                    feed.read_text().replace(
                        prefix, f"http://127.0.0.1:{server.server_port}/{scenario}/"
                    )
                )
                run(str(sparkle / "bin/sign_update"), "--ed-key-file", "-", str(feed), input=key)
                if mode == "tampered":
                    with archive.open("ab") as output:
                        output.write(b"tampered")
                process = subprocess.Popen(
                    [apps[0] / "Contents/MacOS/Fixture"],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                try:
                    deadline = time.monotonic() + 60
                    while time.monotonic() < deadline:
                        events = log.read_text()
                        if mode == "install" and "relaunched" in events:
                            break
                        if mode == "quit" and process.poll() is not None:
                            installed = plistlib.loads(
                                (apps[0] / "Contents/Info.plist").read_bytes()
                            )["CFBundleVersion"]
                            if installed == "0.1.1":
                                break
                        elif mode != "install" and process.poll() is not None:
                            break
                        time.sleep(0.1)
                    else:
                        raise AssertionError(f"{mode} timed out; events: {log.read_text()}")
                    if mode == "cancel":
                        time.sleep(
                            1
                        )  # Detect an installer that incorrectly survived ordinary quit.
                    events = log.read_text()
                    version = plistlib.loads((apps[0] / "Contents/Info.plist").read_bytes())[
                        "CFBundleVersion"
                    ]
                    if mode == "tampered":
                        assert (
                            "event:7:3" in events
                            and "event:5:" not in events
                            and version == "0.1.0"
                        ), events
                    elif mode == "cancel":
                        assert (
                            "cancelled" in events and "event:8:" in events and version == "0.1.0"
                        ), events
                    elif mode == "quit":
                        assert (
                            "normal-quit" in events
                            and "confirmed" not in events
                            and "relaunched" not in events
                            and version == "0.1.1"
                        ), events
                        # A later explicit launch uses the updated bundle without an update prompt.
                        run(str(apps[0] / "Contents/MacOS/Fixture"))
                        assert "relaunched" in log.read_text()
                    else:
                        assert (
                            "confirmed" in events and "relaunched" in events and version == "0.1.1"
                        ), events
                    if signed_new and version == "0.1.1":
                        verify_release_signature(
                            apps[0], fingerprint, plist["CFBundleIdentifier"], scenario
                        )
                    print(f"{scenario}: passed", flush=True)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
        finally:
            server.shutdown()


if __name__ == "__main__":
    main()
