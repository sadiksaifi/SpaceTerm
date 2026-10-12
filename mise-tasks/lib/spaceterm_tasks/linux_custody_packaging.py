"""Build the native APT prerequisite without maintainer scripts or host installation."""

import os
import shutil
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, package_server
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.macos_signing import signing_environment
from spaceterm_tasks.packaging import checked


def build_publisher_package(root, destination):
    """Copy manifest-bound bytes into a root-owned Debian package, not native authority.

    The configured APT source must authenticate this exact package before first
    privileged execution. No server state, service unit, or credential is created.
    """
    with (root / "installation.json").open("rb") as source:
        data = source.read(artifacts.MAX_MANIFEST_BYTES + 1)
    manifest = package_server.validate_manifest(
        artifacts.strict_json(data, artifacts.MAX_MANIFEST_BYTES)
    )
    if manifest["platform"] != "linux-x86_64" or manifest["build_identity"] == "development":
        raise TaskError("native publisher package requires Linux Release or Preflight")
    name = "spaceterm-native-publisher" + (
        "-preflight" if manifest["build_identity"] == "preflight" else ""
    )
    version = manifest["version"] if manifest["build_identity"] == "release" else "0.0.0~preflight"
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() or destination.is_symlink():
        raise TaskError("native publisher package output already exists")
    with tempfile.TemporaryDirectory(
        prefix="native-publisher-", dir=destination.parent
    ) as temporary:
        stage = Path(temporary) / "package"
        installed = stage / "usr/lib" / name
        installed.mkdir(parents=True)
        (installed / "installation.json").write_bytes(data)
        (installed / "installation.json").chmod(0o444)
        for file in manifest["files"]:
            target = installed / file["name"]
            package_server.copy_file(root / file["name"], target, file["mode"])
            if target.stat().st_size != file["size"] or artifacts.digest(target) != file["sha256"]:
                raise TaskError("native publisher copy differs from its installation manifest")
        spawner = installed / "libexec/spaceterm-server-spawner"
        if not spawner.is_file() or not spawner.stat().st_mode & 0o111:
            raise TaskError("native publisher package requires the authenticated spawner")
        # Normalize directory permissions independently of the builder's umask.
        # dpkg-deb records root ownership; account-created staging inodes are not
        # approved installation inodes. Native publication still creates fresh ones.
        for path in [stage, *stage.rglob("*")]:
            if path.is_dir():
                path.chmod(0o755)
        control = stage / "DEBIAN"
        control.mkdir()
        control.chmod(0o755)
        (control / "control").write_text(
            f"Package: {name}\nVersion: {version}\nArchitecture: amd64\n"
            "Maintainer: SpaceTerm\nSection: admin\nPriority: optional\n"
            "Depends: libc6 (>= 2.35), libgcc-s1\n"
            "Description: Authenticated SpaceTerm native publisher prerequisite\n"
            " Contains protected native management and execution code, not an account installation.\n"
        )
        (control / "control").chmod(0o644)
        output = Path(temporary) / "publisher.deb"
        checked(
            ["dpkg-deb", "--build", "--root-owner-group", "-Zgzip", stage, output],
            "native publisher package construction failed",
            env=signing_environment(os.environ),
            timeout=120,
        )
        if not output.is_file() or not 0 < output.stat().st_size <= artifacts.MAX_ARCHIVE_BYTES:
            raise TaskError("native publisher package exceeds its bound or is unavailable")
        with output.open("rb") as source, destination.open("xb") as target:
            shutil.copyfileobj(source, target)
    return destination
