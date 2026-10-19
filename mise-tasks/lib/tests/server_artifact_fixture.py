"""Real independent signed server artifacts for native desktop packaging tests."""

import json
import plistlib
import shutil
import subprocess
import sys

from spaceterm_tasks import macos_signing, release
from spaceterm_tasks.package_linux import write_archive

SEED = bytes(range(32))


def configure_trust(root):
    path = root / "packaging/macos/spaceterm/Info.plist"
    info = plistlib.loads(path.read_bytes())
    info["SUPublicEDKey"] = release.derived_public_key(SEED)
    path.write_bytes(plistlib.dumps(info))


def prepare(root, environment, release_tag=None, fingerprint=None, credentials=None):
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=root, check=True, capture_output=True, text=True
    ).stdout.strip()
    expected = {
        "schema": 1,
        "identity": "spaceterm" if release_tag else "spaceterm-preflight",
        "target": "aarch64-apple-darwin"
        if sys.platform == "darwin"
        else "x86_64-unknown-linux-gnu",
        "commit": commit,
        "version": release_tag[1:] if release_tag else "dev." + commit[:12],
    }
    directory = root / "target/server-artifact-fixture"
    shutil.rmtree(directory, ignore_errors=True)
    tree = directory / "server"
    (tree / "bin").mkdir(parents=True)
    executable = tree / "bin/spaceterm-server"
    source = f"""
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (argc == 2 && strcmp(argv[1], "--build-info") == 0) {{
        puts({json.dumps(json.dumps(expected))});
        return 0;
    }}
    if (argc == 2 && strcmp(argv[1], "--check") == 0) {{
        puts("Installed server PTY and shell resources verified.");
        return 0;
    }}
    return 1;
}}
"""
    compiler = (
        ["xcrun", "clang", "-arch", "arm64", "-mmacosx-version-min=26.0"]
        if sys.platform == "darwin"
        else ["cc"]
    )
    subprocess.run(
        [*compiler, "-x", "c", "-", "-o", str(executable)],
        input=source,
        text=True,
        capture_output=True,
        check=True,
    )
    if sys.platform == "darwin":
        if fingerprint is None or credentials is None:
            raise ValueError("the native package fixture requires its disposable signing identity")
        identifier = f"io.github.sadiksaifi.{expected['identity']}.server"
        requirement = f'=designated => identifier "{identifier}" and {macos_signing.certificate_requirement(fingerprint)}'
        with macos_signing.release_keychain(fingerprint, credentials):
            subprocess.run(
                [
                    "codesign",
                    "--force",
                    "--sign",
                    fingerprint,
                    "--identifier",
                    identifier,
                    "--requirements",
                    requirement,
                    "--timestamp=none",
                    str(executable),
                ],
                check=True,
                capture_output=True,
            )
    archive, signature = archive_tree(root, tree, expected)
    environment["SPACETERM_SERVER_ARCHIVE"] = str(archive)
    environment["SPACETERM_SERVER_SIGNATURE"] = str(signature)
    return archive, signature


def archive_tree(root, tree, expected):
    resources = tree / "share/spaceterm"
    shutil.copytree(root / "assets/shell-integration", resources / "shell-integration")
    shutil.copyfile(root / "assets/THIRD-PARTY-NOTICES.txt", resources / "THIRD-PARTY-NOTICES.txt")
    subprocess.run(
        [
            "tic",
            "-x",
            "-o",
            str(resources / "terminfo"),
            str(root / "assets/terminfo/xterm-spaceterm.terminfo"),
        ],
        check=True,
        capture_output=True,
    )
    (tree / "build.json").write_text(json.dumps(expected))
    archive = tree.parent / "artifact.tar.gz"
    signature = tree.parent / "artifact.sig"
    write_archive(tree, archive)
    release.sign_ed25519(SEED, archive, signature)
    return archive, signature
