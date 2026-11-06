"""Real independent signed server artifacts for native desktop packaging tests."""

import json
import plistlib
import shutil
import subprocess
import sys

from spaceterm_tasks import core_package, macos_codesign, update_signing
from spaceterm_tasks.core_builds import _source
from spaceterm_tasks.package_linux import write_archive

SEED = bytes(range(32))


BACKUP_SEED = bytes(range(1, 33))


def configure_trust(root):
    """Trust SEED as the current update key and BACKUP_SEED as the backup, as a release does."""
    current = update_signing.derived_public_key(SEED)
    (root / update_signing.TRUST).write_text(
        json.dumps({"current": current, "backup": update_signing.derived_public_key(BACKUP_SEED)})
    )
    path = root / "packaging/macos/spaceterm/Info.plist"
    info = plistlib.loads(path.read_bytes())
    info["SUPublicEDKey"] = current
    path.write_bytes(plistlib.dumps(info))


def prepare(
    root,
    environment,
    release_tag=None,
    *,
    include_catalog=False,
):
    """A signed Core fixture. A release fixture is a dry run's: ad hoc under the hardened
    runtime, signed with SEED."""
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=root, check=True, capture_output=True, text=True
    ).stdout.strip()
    expected = {
        "schema": 1,
        "source_sha256": _source(root)[2] if include_catalog and not release_tag else None,
        "protocols": core_package.PROTOCOLS,
        "identity": "spaceterm" if release_tag else "spaceterm-preflight",
        "target": "aarch64-apple-darwin"
        if sys.platform == "darwin"
        else "x86_64-unknown-linux-gnu",
        "commit": commit,
        "version": release_tag[1:] if release_tag else "dev." + commit[:12],
        "floor": release_tag[1:] if release_tag else None,
    }
    directory = root / "target/core-artifact-fixture"
    shutil.rmtree(directory, ignore_errors=True)
    tree = directory / "core"
    executable = tree / core_package.executable_path(expected["target"], expected["identity"])
    executable.parent.mkdir(parents=True)
    source = f"""
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (argc == 3 && strcmp(argv[1], "internal") == 0 && strcmp(argv[2], "build-info") == 0) {{
        puts({json.dumps(json.dumps(expected))});
        return 0;
    }}
    if (argc == 3 && strcmp(argv[1], "internal") == 0 && strcmp(argv[2], "check") == 0) {{
        puts("SpaceTerm Core checks passed.");
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
        identifier = core_package.core_identifier(expected["identity"])
        executable = tree / core_package.signed_path(expected["target"], expected["identity"])
        # Release fixtures are a dry run: ad hoc, as the release Team ID cannot be faked.
        macos_codesign.sign_core(executable, macos_codesign.ADHOC, identifier)
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
        (local / "artifact.sha256").write_text(update_signing.sha256(archive) + "\n")
        target = "x86_64-unknown-linux-gnu" if sys.platform == "darwin" else "aarch64-apple-darwin"
        foreign = directory / "foreign/core"
        executable = foreign / core_package.executable_path(target, expected["identity"])
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
        (output / "artifact.sha256").write_text(update_signing.sha256(other_archive) + "\n")
        environment["SPACETERM_CLI_ARTIFACTS"] = str(catalog)
    return archive, signature


def populate(root, tree, expected):
    """Place the resources, and on macOS the Info.plist, in the target's layout."""
    resources = tree / core_package.resource_directory(expected["target"], expected["identity"])
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
        bundle = tree / core_package.bundle_directory(expected["identity"])
        info = bundle / "Contents/Info.plist"
        info.write_bytes(plistlib.dumps(core_package.core_info(expected["identity"])))
        for name in core_package.ICON_RESOURCES:
            (resources / name).write_bytes(b"compiled icon")


def seal_foreign(tree, expected):
    """Stand in for the seal a foreign target's codesign writes, which catalogs never execute."""
    for name in core_package.bundle_files(expected["target"], expected["identity"]):
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
        else core_package.artifact_signing_seed(expected["identity"])
    )
    update_signing.sign(seed, archive, signature)
    return archive, signature
