#!/usr/bin/env python
# MISE description="Exercise authenticated updates and certificate-signed installs in isolated bundles"
"""Exercise Sparkle signature rejection, cancellation, installation, relaunch and Stepping
Stones in temp apps.

Each scenario publishes its releases through the release task's appcast writer, so every
appcast holds one item per step. The stepping-stone scenario installs from below the newest
release's Update Floor and must land on the floor release, not the newest one.
"""

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

from spaceterm_tasks import ROOT, macos_codesign, release, release_feed
from spaceterm_tasks import sparkle as SPARKLE
from spaceterm_tasks.macos_signing import release_keychain
from tests.macos_signing_fixture import NAME, certificate


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, **kwargs).stdout


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass


def verify_certificate(app, fingerprint):
    """The self-signed fixture has no Team ID, so pin its certificate throughout the bundle."""
    run(
        "codesign",
        "--verify",
        "--strict",
        "--deep",
        "-R",
        f'=certificate leaf = H"{fingerprint.lower()}"',
        str(app),
    )


def main():
    sparkle = SPARKLE.directory()
    with (
        tempfile.TemporaryDirectory(prefix="spaceterm-updater-test-") as temporary,
        ExitStack() as signing,
    ):
        root = Path(temporary)
        fingerprint, credentials = certificate(root / "signing")
        signing.enter_context(release_keychain(NAME, {**os.environ, **credentials}))
        core = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0), functools.partial(QuietHandler, directory=root)
        )
        thread = threading.Thread(target=core.serve_forever, daemon=True)
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

            def sparkle_run(tool, *arguments):
                run(str(tool), "--ed-key-file", "-", *map(str, arguments), input=key)

            trust = (base64.b64encode(public).decode(),)
            for scenario, mode, signed, versions in (
                ("tampered", "tampered", False, ("0.1.0", "0.1.1")),
                ("cancel", "cancel", False, ("0.1.0", "0.1.1")),
                ("install", "install", False, ("0.1.0", "0.1.1")),
                ("quit", "quit", False, ("0.1.0", "0.1.1")),
                ("certificate-install", "install", True, ("0.1.0", "0.1.1")),
                # 0.1.2's floor is 0.1.1, so 0.1.0 must step to 0.1.1 first.
                ("stepping-stone", "install", False, ("0.1.0", "0.1.1", "0.1.2")),
            ):
                directory = root / scenario
                directory.mkdir()
                log = directory / "events"
                log.touch()
                feed_url = f"http://127.0.0.1:{core.server_port}/{scenario}/appcast.xml"
                apps = []
                for version in versions:
                    app = directory / f"app-{version}" / "SpaceTermUpdateTest.app"
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
                        "CFBundleIdentifier": f"com.peblon.spaceterm.updater-test.{scenario}",
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
                    # The release order: Sparkle's code inside out, then the app.
                    macos_codesign.sign_desktop(app, NAME if signed else "-", entitlements)
                    if signed:
                        verify_certificate(app, fingerprint)
                    apps.append(app)
                # Publish each release in order: the first is its own floor, and each later
                # one's floor is the release before it.
                previous = None
                for index, version in enumerate(versions):
                    published = directory / f"v{version}"
                    published.mkdir()
                    run(
                        "hdiutil",
                        "create",
                        "-quiet",
                        "-format",
                        "UDZO",
                        "-srcfolder",
                        str(apps[index].parent),
                        str(published / release_feed.dmg_name(version)),
                    )
                    floor = versions[max(index - 1, 0)]
                    feed = release.write_appcast(published, version, floor, previous, sparkle_run)
                    # The next release continues this one's appcast, as the release task does
                    # with the floor release's published appcast.
                    previous = release.authenticated_appcast(feed.read_bytes(), trust)
                archive = directory / "v0.1.1" / release_feed.dmg_name("0.1.1")
                # Exercise the production writer and verifier, then serve the fixture locally.
                feed = directory / "appcast.xml"
                feed.write_text(
                    (directory / f"v{versions[-1]}" / "appcast.xml")
                    .read_text()
                    .replace(
                        f"{release_feed.RELEASES}/download/",
                        f"http://127.0.0.1:{core.server_port}/{scenario}/",
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
                        # One Relaunch quits through the quit policy and relaunches, with no
                        # dismissal and no second click.
                        assert (
                            events.count("confirmed") == 1
                            and "relaunch-quit" in events
                            and "dismissed" not in events
                            and "relaunched" in events
                            and version == "0.1.1"
                        ), events
                    if signed and version == "0.1.1":
                        verify_certificate(apps[0], fingerprint)
                    print(f"{scenario}: passed", flush=True)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
        finally:
            core.shutdown()


if __name__ == "__main__":
    main()
