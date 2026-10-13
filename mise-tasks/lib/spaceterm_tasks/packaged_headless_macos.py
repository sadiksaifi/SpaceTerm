"""Run signed archive queries outside the checkout without installing a service.

Ad hoc signing is explicit test composition. It never qualifies a candidate for
installation or release. No Keychain item, service registration or host setting
is changed. Query execution cannot read host account files or use network or Mach services.
"""

import argparse
import json
import os
import plistlib
import re
import sys
import tarfile
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, main, package_server, package_server_macos, server_artifacts
from spaceterm_tasks import server_vm_linux as native


def require_host():
    if (
        sys.platform != "darwin"
        or os.getuid() == 0
        or os.geteuid() == 0
        or os.uname().machine != "arm64"
    ):
        raise TaskError("unsupported-headless-host")


def require_revision(source_commit):
    if (
        not isinstance(source_commit, str)
        or re.fullmatch(r"[0-9a-f]{40}", source_commit) is None
        or source_commit == "0" * 40
    ):
        raise TaskError("invalid-source-commit")


def signature_details(path, identifier):
    # codesign writes display records to stderr. A fixed shell redirect lets the
    # existing bounded, child-owning executor read them without a second runner.
    details = native.run_tool(
        [
            "/bin/sh",
            "-c",
            'exec /usr/bin/codesign "$@" 2>&1',
            "--",
            "--display",
            "--verbose=4",
            path,
        ],
        timeout=30,
        maximum=16384,
    )
    identifiers = re.findall(rb"(?m)^Identifier=(.*)$", details)
    hashes = re.findall(rb"(?m)^CDHash=([0-9a-f]{40})$", details)
    flags = re.search(rb"\bflags=0x([0-9a-fA-F]+)\b", details)
    if (
        identifiers != [identifier.encode()]
        or len(hashes) != 1
        or flags is None
        or not int(flags.group(1), 16) & 0x10000
    ):
        raise TaskError("invalid-packaged-signature")
    entitlements = native.run_tool(
        ["/usr/bin/codesign", "--display", "--entitlements", ":-", path],
        timeout=30,
        maximum=65536,
    )
    try:
        server_artifacts.validate_entitlements(
            plistlib.loads(entitlements) if entitlements.strip() else {}
        )
    except plistlib.InvalidFileException:
        raise TaskError("invalid-packaged-entitlements") from None
    return hashes[0].decode(), b"\nSignature=adhoc\n" in details


def verify_signature(app, metadata, *, allow_ad_hoc):
    identifier = metadata["native_identifier"]
    binary = app / "Contents/MacOS/spaceterm-server"
    cdhash, ad_hoc = signature_details(app, identifier)
    binary_hash, binary_ad_hoc = signature_details(binary, identifier)
    if cdhash != binary_hash or ad_hoc != binary_ad_hoc:
        raise TaskError("invalid-packaged-signature")
    if ad_hoc:
        if allow_ad_hoc is not True or metadata["build_identity"] == "release":
            raise TaskError("ad-hoc-signature-not-approved")
        for path in (binary, app):
            native.run_tool(
                [
                    "/usr/bin/codesign",
                    "--verify",
                    "--strict",
                    "--deep",
                    "-R",
                    f'=identifier "{identifier}"',
                    path,
                ],
                timeout=30,
                maximum=16384,
            )
        architecture = native.run_tool(["/usr/bin/lipo", "-archs", binary], maximum=4096)
        if architecture.strip() != b"arm64":
            raise TaskError("unsupported-packaged-architecture")
    else:
        package_server_macos.verify_server_signature(app, identifier)
    return cdhash, "ad-hoc" if ad_hoc else "pinned-certificate"


def sandbox_profile(package, executable, home, runtime, working):
    def literal(path):
        return json.dumps(str(path), ensure_ascii=False)

    # Do not allow process-fork, other executables, network or Mach lookups.
    # System libraries remain available; the existing Mach-O closure policy
    # independently rejects graphical frameworks and loader-relative libraries.
    return (
        "(version 1)(deny default)"
        f"(allow process-exec (literal {literal(executable)}))"
        "(allow sysctl-read)"
        "(allow file-read-metadata)"
        f"(allow file-map-executable (subpath {literal(package)})"
        '(subpath "/System/Library")(subpath "/usr/lib"))'
        "(allow file-read* "
        f"(subpath {literal(package)})(subpath {literal(home)})(subpath {literal(runtime)})"
        f'(subpath {literal(working)})(literal "/")'
        '(subpath "/System/Library")(subpath "/usr/lib")'
        '(literal "/dev/null")(literal "/dev/urandom")(literal "/dev/random"))'
        f"(allow file-write* (subpath {literal(home)})(subpath {literal(runtime)}))"
    )


def run(archive, source_commit, *, allow_ad_hoc=False):
    """Return bounded query evidence. Never execute serve or lifecycle commands."""
    require_host()
    require_revision(source_commit)
    package_server.verify_archive(archive)
    with tempfile.TemporaryDirectory(prefix="spaceterm-packaged-macos-") as temporary:
        directory = Path(temporary).resolve()
        # Archive data and extraction live in a new directory. Neither it nor
        # its contents are Python's working directory or import search path.
        snapshot = directory / "archive.tar.gz"
        package_server.copy_file(archive, snapshot, 0o600)
        manifest = package_server.verify_archive(snapshot)
        expected = {key: manifest[key] for key in server_artifacts.METADATA_FIELDS}
        server_artifacts.validate_metadata(expected, "darwin-arm64")
        extracted = directory / "extracted"
        extracted.mkdir(mode=0o700)
        with tarfile.open(snapshot, "r:gz") as source:
            source.extractall(extracted, filter="data")
        package = extracted / "spaceterm-server"
        if manifest["entry_points"]["cli"] is not None or (package / "bin/spaceterm-cli").exists():
            raise TaskError("packaged-client-present")
        app = package / server_artifacts.bundle_name(expected["build_identity"])
        executable = app / "Contents/MacOS/spaceterm-server"
        package_server_macos.verify_info_plist(app, expected)
        package_server_macos.verify_macho_closure(package, {executable})
        cdhash, signature = verify_signature(app, expected, allow_ad_hoc=allow_ad_hoc)
        home, runtime, working = (directory / name for name in ("home", "runtime", "working"))
        for path in (home, runtime, working):
            path.mkdir(mode=0o700)
        profile = sandbox_profile(package, executable, home, runtime, working)
        environment = {
            "HOME": str(home),
            "XDG_DATA_HOME": str(home / "data"),
            "XDG_RUNTIME_DIR": str(runtime),
            "TMPDIR": str(runtime),
            "LC_ALL": "C",
        }

        def query(arguments, *, reject_stderr=False):
            return native.run_tool(
                ["/usr/bin/sandbox-exec", "-p", profile, executable, *arguments],
                timeout=5,
                maximum=4096,
                cwd=working,
                environment=environment,
                reject_stderr=reject_stderr,
            )

        actual = server_artifacts.validate_metadata(
            server_artifacts.strict_json(
                query(["--distribution-metadata"], reject_stderr=True), 4096
            ),
            "darwin-arm64",
        )
        if actual != expected:
            raise TaskError("packaged-metadata-mismatch")
        help_text = query(["--help"], reject_stderr=True)
        version = query(["--version"], reject_stderr=True)
        if (
            b"usage: spaceterm-server" not in help_text
            or version != ("spaceterm-server " + expected["version"] + "\n").encode()
        ):
            raise TaskError("packaged-query-mismatch")
        invalid = (
            [],
            ["--unknown"],
            ["--distribution-metadata", "unexpected-argument"],
            ["serve", "unexpected-argument"],
        )
        for arguments in invalid:
            try:
                query(arguments)
            except TaskError as error:
                if str(error) != "tool-failed":
                    raise
            else:
                raise TaskError("invalid-packaged-arguments-accepted")
        if any(home.iterdir()) or any(runtime.iterdir()) or any(working.iterdir()):
            raise TaskError("packaged-query-mutated-state")
        version = native.run_tool(["/usr/bin/sw_vers", "-productVersion"], maximum=4096)
        if re.fullmatch(rb"[0-9]+(?:\.[0-9]+){1,2}\n", version) is None:
            raise TaskError("unsupported-headless-host-versions")
        return {
            "schema": 1,
            "classification": "packaged-queries-passed",
            "source_commit": source_commit,
            "archive_sha256": server_artifacts.digest(snapshot),
            "metadata": actual,
            "host": {"macos": version.decode().strip(), "architecture": "arm64"},
            "uid": os.getuid(),
            "cdhash": cdhash,
            "signature": signature,
            "release_signature_verified": signature == "pinned-certificate",
            "queries": 3 + len(invalid),
            "service_lifecycle_tested": False,
            "runtime_readiness_tested": False,
        }


def prepare_archive(executable, destination):
    """Build a disposable non-release archive with no signing identity or Keychain."""
    require_host()
    metadata = package_server.executable_metadata(executable, "darwin-arm64", None)
    if metadata["build_identity"] == "release":
        raise TaskError("release-ad-hoc-signing-refused")
    with tempfile.TemporaryDirectory(prefix="spaceterm-adhoc-server-") as temporary:
        root = Path(temporary).resolve() / "spaceterm-server"
        app = root / server_artifacts.bundle_name(metadata["build_identity"])
        info = plistlib.loads(package_server_macos.TEMPLATE.read_bytes())
        for key, value in list(info.items()):
            if isinstance(value, str):
                info[key] = (
                    value.replace("@SERVER_IDENTIFIER@", metadata["native_identifier"])
                    .replace("@SERVER_NAME@", app.stem)
                    .replace("@BUNDLE_VERSION@", "0.0.0")
                )
        (app / "Contents").mkdir(parents=True)
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        copied = package_server.copy_built_executables(
            {"spaceterm-server": executable}, root, metadata
        )
        package_server.copy_resources(app / "Contents/Resources")
        package_server_macos.verify_macho_closure(root, set(copied))
        for path in (*copied, app):
            native.run_tool(
                [
                    "/usr/bin/codesign",
                    "--force",
                    "--sign",
                    "-",
                    "--identifier",
                    metadata["native_identifier"],
                    "--options",
                    "runtime",
                    "--entitlements",
                    package_server_macos.ENTITLEMENTS,
                    "--timestamp=none",
                    path,
                ],
                timeout=30,
                maximum=16384,
            )
        verify_signature(app, metadata, allow_ad_hoc=True)
        package_server_macos.verify_info_plist(app, metadata)
        package_server.verify_executable_metadata(root, metadata)
        package_server.write_manifest(root, metadata)
        package_server.write_archive(root, destination)
        package_server.verify_archive(destination)
    return metadata


class _Parser(argparse.ArgumentParser):
    def error(self, message):
        raise TaskError("invalid-headless-arguments")


def cli(arguments):
    parser = _Parser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--allow-ad-hoc", action="store_true")
    args, unknown = parser.parse_known_args(arguments)
    if unknown:
        raise TaskError("invalid-headless-arguments")
    result = run(args.archive, args.source_commit, allow_ad_hoc=args.allow_ad_hoc)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


def prepare_cli(arguments):
    parser = _Parser(description=prepare_archive.__doc__)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args, unknown = parser.parse_known_args(arguments)
    if unknown:
        raise TaskError("invalid-headless-arguments")
    prepare_archive(args.executable, args.output)
    print("classification=ad-hoc-test-archive-created release_signature_verified=false")
    return 0


if __name__ == "__main__":
    main(lambda: cli(sys.argv[1:]))
