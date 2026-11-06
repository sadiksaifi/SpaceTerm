"""Bind finalized Core packages to the public inputs of an already-compiled desktop."""

import base64
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, update_signing

SIGNING_SECRET = "SPACETERM_RELEASE_UPDATE_ED25519_SEED"


def compile_environment(environment, release_tag):
    """Public desktop build inputs. No signed Core package is read or built here."""
    from spaceterm_tasks.core_builds import _source
    from spaceterm_tasks.core_package import core_floor

    result = dict(environment)
    result["SPACETERM_CLI_SOURCE_STATE"] = _source(ROOT)[2]
    if release_tag:
        result["SPACETERM_RELEASE_FLOOR"] = core_floor(release_tag)
    return result


def compiled_desktop(compiled, environment, build):
    """Read a prior compile artifact, or compile locally and collect the same metadata."""
    if compiled is None:
        executable, metadata = build(environment)
    else:
        directory = Path(compiled) / "desktop"
        executable = directory / "spaceterm-desktop"
        try:
            metadata = (directory / "desktop-build-info.json").read_bytes()
        except OSError:
            raise TaskError("the compiled desktop metadata is unavailable") from None
        if not executable.is_file():
            raise TaskError("the compiled desktop executable is unavailable")
    try:
        info = json.loads(metadata)
        if info["schema"] != 1 or set(info) != {"schema", "target", "update_trust", "build"}:
            raise ValueError
    except (ValueError, KeyError, TypeError):
        raise TaskError("the compiled desktop metadata is malformed") from None
    return executable, info


def validate_compiled(compiled_info, release_tag):
    """Refuse stale compile artifacts before assigning the selected package's release identity."""
    from spaceterm_tasks.core_builds import _source
    from spaceterm_tasks.packaging import checked, release_version

    _, repositories, source = _source(ROOT)
    build = compiled_info["build"]
    if (
        build["commit"] != repositories[Path(".")]
        or build["source_sha256"] != source
        or build["identity"] != ("spaceterm" if release_tag else "spaceterm-preflight")
    ):
        raise TaskError("the compiled desktop differs from the packaging source or identity")
    if release_tag:
        reference = f"refs/tags/{release_tag}"
        kind = checked(
            ["git", "cat-file", "-t", reference], "the package release tag is unavailable", cwd=ROOT
        ).strip()
        commit = (
            checked(
                ["git", "rev-parse", reference + "^{commit}"],
                "the package release commit is unavailable",
                cwd=ROOT,
            )
            .strip()
            .decode()
        )
        if (
            kind != b"tag"
            or commit != build["commit"]
            or build["version"] != release_version(release_tag)
        ):
            raise TaskError("the compiled desktop differs from the selected release tag")


def _seed(compiled_info, environment):
    identity = compiled_info["build"]["identity"]
    if identity != "spaceterm":
        from spaceterm_tasks.core_package import artifact_signing_seed

        return artifact_signing_seed(identity)
    secret = environment.get(SIGNING_SECRET)
    if not secret:
        raise TaskError(f"{SIGNING_SECRET} must hold the manifest signing key")
    seed = update_signing.signing_seed(secret)
    if update_signing.derived_public_key(seed) != compiled_info["update_trust"]["current"]:
        raise TaskError("the manifest signing key differs from compiled desktop trust")
    # Every build compiles tracked public trust. Dry runs commit generated public keys in
    # their disposable checkout before tagging and compiling it.
    if (
        not update_signing.dry_run(environment)
        and tuple(compiled_info["update_trust"][name] for name in ("current", "backup"))
        != update_signing.release_trust()
    ):
        raise TaskError("compiled desktop trust differs from package verification trust")
    return seed


def stage_desktop_manifest(
    staging_tree, packages, compiled_info, *, macos, environment=os.environ, execute: bool = False
):
    """Verify both finalized packages, sign their exact authorization, and stage desktop resources.

    `packages` maps both Rust targets to directories with artifact.tar.gz, artifact.sig and
    artifact.sha256. `compiled_info` is Cargo's decoded desktop-build-info.json. `staging_tree`
    is the unsigned .app or Linux tar root. This operation never compiles code.
    """
    from spaceterm_tasks.core_catalog import FILES, _copy_pair_file, stage_catalog

    if execute:
        require_no_release_secrets(environment)
    from spaceterm_tasks.core_package import TARGETS

    try:
        if set(packages) != set(TARGETS.values()):
            raise TaskError("the desktop manifest requires both finalized Core targets")
        build = compiled_info["build"]
        target = compiled_info["target"]
        if target != TARGETS["darwin" if macos else "linux"]:
            raise TaskError("the compiled desktop target differs from its staging tree")
        seed = _seed(compiled_info, environment)
        resources = Path(staging_tree) / ("Contents/Resources" if macos else "share/spaceterm")
        resources.mkdir(parents=True, exist_ok=True)
        # Prepare everything privately; publish only a completely verified manifest and catalog.
        with tempfile.TemporaryDirectory(dir=resources, prefix=".core-resources-") as temporary:
            temporary = Path(temporary)
            source = temporary / "input"
            source.mkdir()
            for native, package in packages.items():
                output = source / native
                output.mkdir()
                for name, maximum in FILES.items():
                    _copy_pair_file(Path(package) / name, output / name, maximum)
            catalog = temporary / "core-artifacts"
            entries = stage_catalog(
                None,
                None,
                catalog,
                build["identity"],
                source=source,
                require_all=True,
                execute=execute,
            )
            archives = []
            for native, entry in sorted(entries.items()):
                info = entry["build"]
                expected = {
                    name: build[name]
                    for name in ("identity", "commit", "version", "source_sha256", "protocols")
                }
                expected["floor"] = build["core_floor"]
                if any(info[name] != value for name, value in expected.items()):
                    raise TaskError(
                        "a finalized Core package differs from compiled desktop provenance"
                    )
                archives.append(
                    {
                        "target": native,
                        "sha256": entry["sha256"],
                        "signature": base64.b64encode(
                            (catalog / native / "artifact.sig").read_bytes()
                        ).decode(),
                    }
                )
            manifest = catalog / "manifest.json"
            manifest.write_text(
                json.dumps({"schema": 1, "build": build, "archives": archives}, sort_keys=True)
                + "\n"
            )
            update_signing.sign(seed, manifest, catalog / "manifest.sig")
            manifest.chmod(0o644)
            (catalog / "manifest.sig").chmod(0o644)
            update_signing.verify(
                (update_signing.derived_public_key(seed),), manifest, catalog / "manifest.sig"
            )
            local_pair = temporary / "core-artifact"
            local_pair.mkdir()
            for name in ("artifact.tar.gz", "artifact.sig"):
                shutil.copyfile(catalog / target / name, local_pair / name)
            for name in ("core-artifacts", "core-artifact"):
                destination = resources / name
                if destination.exists() or destination.is_symlink():
                    raise TaskError("the desktop Core resource destination already exists")
            (temporary / "core-artifacts").rename(resources / "core-artifacts")
            local_pair.rename(resources / "core-artifact")
        return resources / "core-artifacts/manifest.json"
    except (OSError, KeyError, TypeError, ValueError):
        raise TaskError("the desktop Core manifest could not be staged") from None


def prepare_resources(staging_tree, compiled_info, release_tag, *, macos, execute: bool = False):
    """Select finalized release packages or prepare local nonproduction packages after Cargo."""
    from spaceterm_tasks.core_package import TARGETS, prepare_desktop_artifact

    source = os.environ.get("SPACETERM_CLI_ARTIFACTS")
    if source:
        packages = {target: Path(source) / target for target in TARGETS.values()}
        return stage_desktop_manifest(
            staging_tree, packages, compiled_info, macos=macos, execute=execute
        )
    if release_tag:
        raise TaskError(
            "release desktop packaging needs both Core builds in SPACETERM_CLI_ARTIFACTS"
        )
    if not execute:
        raise TaskError("static desktop packaging requires finalized Core packages")
    with tempfile.TemporaryDirectory(prefix="spaceterm-desktop-core-") as temporary:
        temporary = Path(temporary)
        catalog = temporary / "catalog"
        prepare_desktop_artifact(
            temporary / "local", compiled_info["build"]["identity"], catalog_destination=catalog
        )
        packages = {target: catalog / target for target in TARGETS.values()}
        return stage_desktop_manifest(
            staging_tree, packages, compiled_info, macos=macos, execute=execute
        )


def require_no_release_secrets(environment=os.environ):
    from spaceterm_tasks.macos_signing import SIGNING_INPUTS

    names = (
        *SIGNING_INPUTS,
        "SPACETERM_RELEASE_DEVID_P12_BASE64",
        "SPACETERM_RELEASE_DEVID_P12_PASSWORD",
        "SPACETERM_RELEASE_NOTARY_P8",
        "HOMEBREW_TAP_PAT",
    )
    if any(os.environ.get(name) or environment.get(name) for name in names):
        raise TaskError("product execution requires a credential-free verification job")


def verify_embedded_resources(resources, identity, *, execute=False, executable=None):
    """Static resource authorization; executable probes run only in the no-secret lane."""
    try:
        from spaceterm_tasks.core_catalog import _copy_pair_file, _snapshot, stage_catalog
        from spaceterm_tasks.core_package import MAX_ARCHIVE, TARGETS, artifact_trust, host_target

        if execute:
            require_no_release_secrets()
        resources = Path(resources)
        with tempfile.TemporaryDirectory(prefix="spaceterm-embedded-verify-") as temporary:
            temporary = Path(temporary)
            source = temporary / "input"
            source.mkdir()
            _snapshot(resources / "core-artifacts", source, require_all=True)
            for name, maximum in (("manifest.json", 16384), ("manifest.sig", 64)):
                _copy_pair_file(resources / "core-artifacts" / name, temporary / name, maximum)
            update_signing.verify(
                artifact_trust(identity), temporary / "manifest.json", temporary / "manifest.sig"
            )
            manifest = json.loads((temporary / "manifest.json").read_bytes())
            if manifest["schema"] != 1 or manifest["build"]["identity"] != identity:
                raise TaskError("the desktop Core manifest identity is invalid")
            entries = stage_catalog(
                None,
                None,
                temporary / "catalog",
                identity,
                source=source,
                require_all=True,
                execute=execute,
            )
            listed = {entry["target"]: entry for entry in manifest["archives"]}
            if len(manifest["archives"]) != 2 or set(listed) != set(TARGETS.values()):
                raise TaskError("the desktop Core manifest targets are invalid")
            for target, entry in entries.items():
                signature = (source / target / "artifact.sig").read_bytes()
                if (
                    listed[target]["sha256"] != entry["sha256"]
                    or base64.b64decode(listed[target]["signature"], validate=True) != signature
                ):
                    raise TaskError("the embedded Core differs from its signed manifest")
                for key in ("identity", "commit", "version", "source_sha256", "protocols"):
                    if entry["build"][key] != manifest["build"][key]:
                        raise TaskError("the embedded Core build differs from its manifest")
                if entry["build"]["floor"] != manifest["build"]["core_floor"]:
                    raise TaskError("the embedded Core floor differs from its manifest")
            local = temporary / "local"
            local.mkdir()
            for name, maximum in (("artifact.tar.gz", MAX_ARCHIVE), ("artifact.sig", 64)):
                _copy_pair_file(resources / "core-artifact" / name, local / name, maximum)
            native = host_target()
            if (
                update_signing.sha256(local / "artifact.tar.gz") != entries[native]["sha256"]
                or (local / "artifact.sig").read_bytes()
                != (source / native / "artifact.sig").read_bytes()
            ):
                raise TaskError("the local Core differs from the manifest")
            if execute:
                if executable is None:
                    raise TaskError(
                        "desktop executable verification requires the staged executable"
                    )
                result = subprocess.run(
                    [executable, "--build-info"], env={}, capture_output=True, timeout=10
                )
                if result.returncode or result.stderr or len(result.stdout) > 16384:
                    raise TaskError("the desktop verified build-info check failed")
                client = json.loads(result.stdout)
                if (
                    client["commit"] != manifest["build"]["commit"]
                    or client["core_provenance"] != manifest["build"]
                    or client["core_candidate_sha256"] != entries[native]["sha256"]
                ):
                    raise TaskError("the desktop provenance differs from its resources")
            return {
                "build": manifest["build"],
                "core_sha256": {target: entry["sha256"] for target, entry in entries.items()},
            }
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired):
        raise TaskError("desktop artifact verification failed") from None


def verify_desktop_release(artifact, release_tag, *, macos, source_sha, plan_sha256, core_digests):
    """Run final-asset product checks only on a credential-free runner and return evidence."""
    try:
        from spaceterm_tasks import package_linux, package_macos

        require_no_release_secrets()
        from spaceterm_tasks.core_package import TARGETS

        if (
            not isinstance(source_sha, str)
            or not re.fullmatch(r"[0-9a-f]{40}", source_sha)
            or not isinstance(plan_sha256, str)
            or not re.fullmatch(r"[0-9a-f]{64}", plan_sha256)
            or set(core_digests) != set(TARGETS.values())
            or any(
                not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value)
                for value in core_digests.values()
            )
        ):
            raise TaskError("the desktop verification inputs are malformed")
        artifact = Path(artifact)
        digest = update_signing.sha256(artifact)
        result = (
            package_macos.verify(dmg=artifact, release_tag=release_tag, execute=True)
            if macos
            else package_linux.verify(archive=artifact, release_tag=release_tag, execute=True)
        )
        if (
            result["build"]["commit"] != source_sha
            or result["core_sha256"] != core_digests
            or update_signing.sha256(artifact) != digest
        ):
            raise TaskError("the desktop verification evidence differs from the release inputs")
        return {
            "schema": 1,
            "kind": "desktop-verification",
            "source_sha": source_sha,
            "plan_sha256": plan_sha256,
            "asset_sha256": digest,
            "core_sha256": result["core_sha256"],
        }
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired):
        raise TaskError("desktop artifact verification failed") from None
