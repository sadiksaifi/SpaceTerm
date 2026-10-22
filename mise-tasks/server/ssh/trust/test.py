#!/usr/bin/env python
# MISE description="Exercise SSH trust transitions through isolated previous-policy executables"
"""Compile fixture maintenance executables and test the real client SSH update journey."""

import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, macos_signing, release
from tests.macos_signing_fixture import certificate
from tests.server_artifact_fixture import SEED, archive_tree


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, **kwargs)


def exercise(root):
    source = root / "source"
    for directory in ("packaging", "crates/spaceterm-directories", "crates/spaceterm-server"):
        shutil.copytree(ROOT / directory, source / directory)
    shutil.copyfile(ROOT / "Cargo.lock", source / "Cargo.lock")
    info = source / "packaging/macos/spaceterm/Info.plist"
    document = plistlib.loads(info.read_bytes())
    document["SUPublicEDKey"] = release.derived_public_key(SEED)
    info.write_bytes(plistlib.dumps(document))
    fingerprint, credentials = (
        certificate(root / "certificate") if sys.platform == "darwin" else (None, None)
    )
    if fingerprint:
        config = source / "packaging/macos/spaceterm/Packager.toml"
        config.write_text(
            re.sub(
                r'signing-identity = ".*"',
                f'signing-identity = "{fingerprint}"',
                config.read_text(),
            )
        )
    previous = source / "crates/previous-server"
    shutil.copytree(source / "crates/spaceterm-server", previous)
    manifest = previous / "Cargo.toml"
    manifest.write_text(
        manifest.read_text().replace('name = "spaceterm-server"', 'name = "previous-server"', 1)
    )
    runtime = previous / "src/service/runtime.rs"
    text = runtime.read_text()
    # Freeze the previous production-only trust policy in a separate executable. No trust
    # override or fixture hook is added to the product. Both processes use real Ed25519 and,
    # on macOS, the real pinned native certificate requirement.
    start = text.index("fn public_key(")
    end = text.index("fn authenticate(", start)
    text = (
        text[:start]
        + """fn public_key(_: &str) -> Result<Vec<u8>, ServerError> {
    let template = include_str!("../../../../packaging/macos/spaceterm/Info.plist");
    let key = template.split_once("<key>SUPublicEDKey</key>")
        .and_then(|(_, tail)| tail.trim_start().strip_prefix("<string>"))
        .and_then(|tail| tail.split_once("</string>"))
        .ok_or(ServerError::RuntimeUnavailable)?.0;
    base64::engine::general_purpose::STANDARD.decode(key.trim())
        .map_err(|_| ServerError::RuntimeUnavailable)
}
"""
        + text[end:]
    )
    text = text.replace(
        'if identity == "spaceterm" || legacy_production {',
        "if { let _ = legacy_production; true } {",
    )
    runtime.write_text(text)
    for directory, binary, crate in [
        (source / "crates/spaceterm-server", "trust-current", "spaceterm_server"),
        (previous, "trust-previous", "previous_server"),
    ]:
        with (directory / "Cargo.toml").open("a") as output:
            output.write(f'\n[[bin]]\nname = "{binary}"\npath = "fixture_main.rs"\n')
        (directory / "fixture_main.rs").write_text(
            f"fn main() {{ {crate}::service::maintenance::trust_fixture::run(); }}\n"
        )
        with (directory / "src/service/maintenance.rs").open("a") as output:
            output.write("\npub mod trust_fixture;\n")
        destination = directory / "src/service/maintenance/trust_fixture.rs"
        destination.parent.mkdir(exist_ok=True)
        shutil.copyfile(ROOT / "mise-tasks/lib/tests/fixtures/ssh_maintenance.rs", destination)
    (source / "Cargo.toml").write_text(
        '[workspace]\nresolver = "2"\nmembers = ["crates/spaceterm-server", "crates/previous-server", "crates/spaceterm-directories"]\n'
    )
    clean = macos_signing.signing_environment(os.environ)
    run(
        "cargo",
        "build",
        "--offline",
        "--manifest-path",
        str(source / "Cargo.toml"),
        "--target-dir",
        str(ROOT / "target"),
        "--bin",
        "trust-current",
        "--bin",
        "trust-previous",
        env=clean,
    )
    identity = "spaceterm-development"
    target = "aarch64-apple-darwin" if sys.platform == "darwin" else "x86_64-unknown-linux-gnu"
    build = {
        "schema": 1,
        "identity": identity,
        "target": target,
        "commit": "a" * 40,
        "version": "dev." + "a" * 12,
        "protocol_major": 1,
        "capabilities": ["workspace_policy"],
    }
    artifacts = {}
    for policy in ("previous", "current"):
        tree = root / policy / "server"
        (tree / "bin").mkdir(parents=True)
        executable = tree / "bin/spaceterm-server"
        shutil.copyfile(ROOT / f"target/debug/trust-{policy}", executable)
        executable.chmod(0o755)
        if sys.platform == "darwin":
            assert fingerprint is not None and credentials is not None
            identifier = f"io.github.sadiksaifi.{identity}.server"
            if policy == "previous":
                requirement = f'=designated => identifier "{identifier}" and {macos_signing.certificate_requirement(fingerprint)}'
                with macos_signing.release_keychain(fingerprint, credentials):
                    run(
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
                        capture_output=True,
                        env=clean,
                    )
            else:
                run(
                    "codesign",
                    "--force",
                    "--sign",
                    "-",
                    "--identifier",
                    identifier,
                    str(executable),
                    capture_output=True,
                    env=clean,
                )
        archive, signature = archive_tree(ROOT, tree, build)
        if policy == "previous":
            release.sign_ed25519(SEED, archive, signature)
        artifacts[policy] = (archive, signature, release.sha256(archive))
    home = root / "home"
    installations = home / ".local/share" / identity / "server/installations"
    installations.mkdir(parents=True, mode=0o700)
    archive, signature, digest = artifacts["previous"]
    # The previous-policy package was retained before this client existed. Fixture files are
    # published with explicit modes, independent of the machine's ordinary umask.
    retained = installations / digest
    shutil.copytree(root / "previous/server", retained)
    shutil.copyfile(archive, retained / "artifact.tar.gz")
    shutil.copyfile(signature, retained / "artifact.sig")
    for path in [home, *home.rglob("*")]:
        path.chmod(
            0o700 if path.is_dir() else (0o755 if path.name == "spaceterm-server" else 0o600)
        )
    # Authenticated archive members retain their declared package modes.
    for path in [retained, *retained.rglob("*")]:
        path.chmod(0o755 if path.is_dir() or path.name == "spaceterm-server" else 0o644)
    environment = {**clean, "HOME": str(home)}
    run(str(retained / "bin/spaceterm-server"), "fixture-seed", digest, env=environment)
    archive, signature, digest = artifacts["current"]
    catalog = root / "catalog" / target
    catalog.mkdir(parents=True)
    shutil.copyfile(archive, catalog / "artifact.tar.gz")
    shutil.copyfile(signature, catalog / "artifact.sig")
    (catalog / "artifact.sha256").write_text(digest + "\n")
    print("Built isolated production-only and current-policy maintenance executables.", flush=True)
    run(
        "cargo",
        "test",
        "--package",
        "spaceterm",
        "--lib",
        "--all-features",
        "--locked",
        "ssh::server_maintenance::tests::previous_policy_executable_update_transition",
        "--",
        "--ignored",
        "--exact",
        "--nocapture",
        cwd=ROOT,
        env={**clean, "SPACETERM_SSH_TRUST_FIXTURE": str(root)},
    )


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="spaceterm-ssh-trust-") as temporary:
        exercise(Path(temporary).resolve())
