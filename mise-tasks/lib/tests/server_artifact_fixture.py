"""Real independent signed server artifacts for native desktop packaging tests."""

import json
import plistlib
import shutil
import subprocess
import sys

from spaceterm_tasks import macos_signing, release, server_package
from spaceterm_tasks.package_linux import write_archive
from spaceterm_tasks.server_builds import _source

SEED = bytes(range(32))


def configure_trust(root):
    path = root / "packaging/macos/spaceterm/Info.plist"
    info = plistlib.loads(path.read_bytes())
    info["SUPublicEDKey"] = release.derived_public_key(SEED)
    path.write_bytes(plistlib.dumps(info))


def prepare(
    root,
    environment,
    release_tag=None,
    fingerprint=None,
    credentials=None,
    *,
    include_catalog=False,
):
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=root, check=True, capture_output=True, text=True
    ).stdout.strip()
    expected = {
        "schema": 1,
        "source_sha256": _source(root)[2] if include_catalog and not release_tag else None,
        "protocol_major": server_package.PROTOCOL_MAJOR,
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
    executable = tree / server_package.executable_path(expected["target"], expected["identity"])
    executable.parent.mkdir(parents=True)
    source = f"""
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (argc == 4 && strcmp(argv[1], "server") == 0 && strcmp(argv[2], "provision") == 0 && strcmp(argv[3], "build-info") == 0) {{
        puts({json.dumps(json.dumps(expected))});
        return 0;
    }}
    if (argc == 4 && strcmp(argv[1], "server") == 0 && strcmp(argv[2], "provision") == 0 && strcmp(argv[3], "check") == 0) {{
        puts("Server checks passed.");
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
    populate(root, tree, expected)
    if sys.platform == "darwin":
        identifier = server_package.server_identifier(expected["identity"])
        executable = tree / server_package.signed_path(expected["target"], expected["identity"])
        if release_tag:
            if fingerprint is None or credentials is None:
                raise ValueError(
                    "the native package fixture requires its disposable signing identity"
                )
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
        else:
            subprocess.run(
                [
                    "codesign",
                    "--force",
                    "--sign",
                    "-",
                    "--identifier",
                    identifier,
                    "--timestamp=none",
                    str(executable),
                ],
                check=True,
                capture_output=True,
            )
    archive, signature = archive_tree(root, tree, expected)
    environment["SPACETERM_CLI_ARCHIVE"] = str(archive)
    environment["SPACETERM_CLI_SIGNATURE"] = str(signature)
    environment.pop("SPACETERM_CLI_ARTIFACTS", None)
    if release_tag or include_catalog:
        # Catalog verification checks both signed targets and executes only the native one.
        catalog = directory / "catalog"
        local = catalog / expected["target"]
        local.mkdir(parents=True)
        shutil.copyfile(archive, local / "artifact.tar.gz")
        shutil.copyfile(signature, local / "artifact.sig")
        (local / "artifact.sha256").write_text(release.sha256(archive) + "\n")
        target = "x86_64-unknown-linux-gnu" if sys.platform == "darwin" else "aarch64-apple-darwin"
        foreign = directory / "foreign/server"
        executable = foreign / server_package.executable_path(target, expected["identity"])
        executable.parent.mkdir(parents=True)
        if target == "aarch64-apple-darwin":
            header = bytes.fromhex("cffaedfe0c0000010000000002000000") + bytes(16)
        else:
            header = b"\x7fELF\x02\x01\x01" + bytes(9) + bytes.fromhex("03003e00") + bytes(44)
        executable.write_bytes(header)
        executable.chmod(0o755)
        foreign_expected = {**expected, "target": target}
        populate(root, foreign, foreign_expected)
        seal_foreign(foreign, foreign_expected)
        other_archive, other_signature = archive_tree(root, foreign, foreign_expected)
        output = catalog / target
        output.mkdir()
        shutil.copyfile(other_archive, output / "artifact.tar.gz")
        shutil.copyfile(other_signature, output / "artifact.sig")
        (output / "artifact.sha256").write_text(release.sha256(other_archive) + "\n")
        environment["SPACETERM_CLI_ARTIFACTS"] = str(catalog)
    return archive, signature


def populate(root, tree, expected):
    """Place the resources, and on macOS the Info.plist, in the target's layout."""
    resources = tree / server_package.resource_directory(expected["target"], expected["identity"])
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
    if expected["target"] == "aarch64-apple-darwin":
        bundle = tree / server_package.bundle_directory(expected["identity"])
        info = bundle / "Contents/Info.plist"
        info.write_bytes(plistlib.dumps(server_package.server_info(expected["identity"])))


def seal_foreign(tree, expected):
    """Stand in for the seal a foreign target's codesign writes, which catalogs never execute."""
    for name in server_package.bundle_files(expected["target"], expected["identity"]):
        (tree / name).parent.mkdir(parents=True, exist_ok=True)
        (tree / name).write_bytes(b"foreign bundle seal")


def archive_tree(root, tree, expected):
    (tree / "build.json").write_text(json.dumps(expected))
    archive = tree.parent / "artifact.tar.gz"
    signature = tree.parent / "artifact.sig"
    write_archive(tree, archive)
    seed = (
        SEED
        if expected["identity"] == "spaceterm"
        else server_package.artifact_signing_seed(expected["identity"])
    )
    release.sign_ed25519(seed, archive, signature)
    return archive, signature
