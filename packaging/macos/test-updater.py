#!/usr/bin/env python3
"""Exercise Sparkle signature rejection, cancellation, installation and relaunch in temp apps."""

import base64
import functools
import http.server
import importlib.util
import os
import plistlib
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("sparkle", Path(__file__).with_name("prepare-sparkle.py"))
SPARKLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SPARKLE)
ARTIFACT_SPEC = importlib.util.spec_from_file_location("artifacts", Path(__file__).with_name("create-release-assets.py"))
ARTIFACTS = importlib.util.module_from_spec(ARTIFACT_SPEC)
ARTIFACT_SPEC.loader.exec_module(ARTIFACTS)


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, **kwargs).stdout


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def main():
    sparkle = SPARKLE.prepare()
    with tempfile.TemporaryDirectory(prefix="spaceterm-updater-test-") as temporary:
        root = Path(temporary)
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=root))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            seed = os.urandom(32)
            key = base64.b64encode(seed)
            public = run("openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER",
                         input=bytes.fromhex("302e020100300506032b657004220420") + seed)[-32:]
            entitlements = root / "Entitlements.plist"
            entitlements.write_bytes(plistlib.dumps({"com.apple.security.device.audio-input": True,
                                                    "com.apple.security.cs.disable-library-validation": True}))
            binary = root / "fixture"
            run("xcrun", "clang", "-fobjc-arc", "-fblocks", "-mmacosx-version-min=26.0", "-F", str(sparkle),
                "-framework", "Sparkle", "-framework", "AppKit", "-framework", "Foundation",
                "-Wl,-rpath,@executable_path/../Frameworks", str(ROOT / "tests/macos_updater_fixture.m"),
                str(ROOT / "src/platform/macos_updater.m"), "-o", str(binary))
            for mode in ("tampered", "cancel", "install", "quit"):
                directory = root / mode
                directory.mkdir()
                log = directory / "events"
                log.touch()
                feed_url = f"http://127.0.0.1:{server.server_port}/{mode}/appcast.xml"
                apps = []
                for version, name in (("0.1.0", "Installed"), ("0.1.1", "New")):
                    app = directory / name / "SpaceTermUpdateTest.app"
                    contents = app / "Contents"
                    (contents / "MacOS").mkdir(parents=True)
                    (contents / "Frameworks").mkdir()
                    shutil.copy2(binary, contents / "MacOS/Fixture")
                    run("ditto", str(sparkle / "Sparkle.framework"), str(contents / "Frameworks/Sparkle.framework"))
                    plist = {"CFBundleIdentifier": f"io.github.sadiksaifi.spaceterm.updater-test.{mode}",
                             "CFBundleName": "SpaceTermUpdateTest", "CFBundleExecutable": "Fixture",
                             "CFBundlePackageType": "APPL", "CFBundleVersion": version,
                             "CFBundleShortVersionString": version, "LSMinimumSystemVersion": "26.0",
                             "LSUIElement": True, "SUFeedURL": feed_url,
                             "SUPublicEDKey": base64.b64encode(public).decode(),
                             "SUEnableAutomaticChecks": False, "SUAutomaticallyUpdate": False,
                             "SURequireSignedFeed": True, "SUVerifyUpdateBeforeExtraction": True,
                             "NSAppTransportSecurity": {"NSAllowsLocalNetworking": True},
                             "SPTTestLog": str(log), "SPTTestMode": mode}
                    (contents / "Info.plist").write_bytes(plistlib.dumps(plist))
                    # No Apple identity: the fixture deliberately exercises ad hoc application updates.
                    run("codesign", "--force", "--deep", "--sign", "-", "--timestamp=none", "--options", "runtime", "--entitlements", str(entitlements), str(app))
                    apps.append(app)
                archive = directory / "SpaceTerm-0.1.1-darwin-arm64.dmg"
                run("hdiutil", "create", "-quiet", "-format", "UDZO", "-srcfolder", str(apps[1].parent), str(archive))
                prefix = "https://github.com/sadiksaifi/SpaceTerm/releases/download/v0.1.1/"
                run(str(sparkle / "bin/generate_appcast"), "--ed-key-file", "-", "--maximum-deltas", "0",
                    "--maximum-versions", "1", "--download-url-prefix", prefix,
                    "--link", "https://github.com/sadiksaifi/SpaceTerm/releases/tag/v0.1.1", str(directory), input=key)
                feed = directory / "appcast.xml"
                signature = ARTIFACTS.verify_feed(feed, archive, "0.1.1")
                run(str(sparkle / "bin/sign_update"), "--ed-key-file", "-", "--verify", str(archive), signature, input=key)
                run(str(sparkle / "bin/sign_update"), "--ed-key-file", "-", "--verify", str(feed), input=key)
                # Exercise the production generator and verifier, then serve the fixture locally.
                feed.write_text(feed.read_text().replace(prefix, f"http://127.0.0.1:{server.server_port}/{mode}/"))
                run(str(sparkle / "bin/sign_update"), "--ed-key-file", "-", str(feed), input=key)
                if mode == "tampered":
                    with archive.open("ab") as output:
                        output.write(b"tampered")
                process = subprocess.Popen([apps[0] / "Contents/MacOS/Fixture"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    deadline = time.monotonic() + 60
                    while time.monotonic() < deadline:
                        events = log.read_text()
                        if mode == "install" and "relaunched" in events:
                            break
                        if mode == "quit" and process.poll() is not None:
                            installed = plistlib.loads((apps[0] / "Contents/Info.plist").read_bytes())["CFBundleVersion"]
                            if installed == "0.1.1":
                                break
                        elif mode != "install" and process.poll() is not None:
                            break
                        time.sleep(0.1)
                    else:
                        raise AssertionError(f"{mode} timed out; events: {log.read_text()}")
                    if mode == "cancel":
                        time.sleep(1)  # Detect an installer that incorrectly survived ordinary quit.
                    events = log.read_text()
                    version = plistlib.loads((apps[0] / "Contents/Info.plist").read_bytes())["CFBundleVersion"]
                    if mode == "tampered":
                        assert "event:7:3" in events and "event:5:" not in events and version == "0.1.0", events
                    elif mode == "cancel":
                        assert "cancelled" in events and "event:8:" in events and version == "0.1.0", events
                    elif mode == "quit":
                        assert "normal-quit" in events and "confirmed" not in events and "relaunched" not in events and version == "0.1.1", events
                        # A later explicit launch uses the updated bundle without an update prompt.
                        run(str(apps[0] / "Contents/MacOS/Fixture"))
                        assert "relaunched" in log.read_text()
                    else:
                        assert "confirmed" in events and "relaunched" in events and version == "0.1.1", events
                    print(f"{mode}: passed", flush=True)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
        finally:
            server.shutdown()


if __name__ == "__main__":
    main()
