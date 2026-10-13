"""Closed server artifact names, metadata, and headless package policy."""

import hashlib
import json
import re
from pathlib import PurePosixPath

from spaceterm_tasks import TaskError

PLATFORMS = ("darwin-arm64", "linux-x86_64")
BUILDS = {
    "release": ("io.github.sadiksaifi.spaceterm.server", "SpaceTerm Server.app"),
    "preflight": (
        "io.github.sadiksaifi.spaceterm-preflight.server",
        "SpaceTerm Preflight Server.app",
    ),
    "development": (
        "io.github.sadiksaifi.spaceterm-development.server",
        "SpaceTerm Development Server.app",
    ),
}
PINNED_CERTIFICATE = "9E07CD5C15C5C862A21B93EA55D4303BC19B7366"
MAX_FEED_BYTES = 4096
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_EXPANDED_BYTES = 1024 * 1024 * 1024
METADATA_FIELDS = {
    "schema_version",
    "component",
    "build_identity",
    "platform",
    "version",
    "build_commit",
    "protocol_majors",
    "storage",
    "native_identifier",
}
MANIFEST_FIELDS = METADATA_FIELDS | {"entry_points", "files", "dependencies"}
STABLE = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def archive_name(version, platform, build="release"):
    identifier(build)
    if (
        platform not in PLATFORMS
        or not valid_version(version)
        or (build == "release" and not STABLE.fullmatch(version))
    ):
        raise TaskError("invalid server artifact target or version")
    channel = "" if build == "release" else f"-{build}"
    return f"SpaceTerm-Server{channel}-{version}-{platform}.tar.gz"


def feed_name(platform, build="release"):
    identifier(build)
    if platform not in PLATFORMS:
        raise TaskError("unsupported server platform")
    channel = "" if build == "release" else f"-{build}"
    return f"latest-server{channel}-{platform}.json"


def valid_version(version):
    return isinstance(version, str) and (
        bool(STABLE.fullmatch(version))
        or bool(re.fullmatch(r"dev\.[0-9a-f]{12}(\.dirty)?", version))
    )


def identifier(build):
    if build not in BUILDS:
        raise TaskError("unsupported server build identity")
    return BUILDS[build][0]


def bundle_name(build):
    identifier(build)
    return BUILDS[build][1]


def relative_name(name):
    return (
        isinstance(name, str)
        and not any(0xD800 <= ord(c) <= 0xDFFF for c in name)
        and 0 < len(name.encode()) <= 1024
        and not name.startswith("/")
        and "\\" not in name
        and not any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in name)
        and all(part not in ("", ".", "..") for part in name.split("/"))
    )


def strict_json(data, maximum):
    if len(data) > maximum:
        raise TaskError("server metadata exceeds its bound")

    def object_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise TaskError("server metadata contains a duplicate field")
            result[key] = value
        return result

    try:
        result = json.loads(
            data, object_pairs_hook=object_pairs, parse_constant=lambda _: invalid_json()
        )
    except (ValueError, UnicodeError, TypeError, RecursionError):
        raise TaskError("invalid server metadata") from None
    pending = [(result, 0)]
    while pending:
        value, depth = pending.pop()
        if depth > 16:
            raise TaskError("server metadata nesting exceeds its bound")
        if isinstance(value, str) and any(0xD800 <= ord(c) <= 0xDFFF for c in value):
            raise TaskError("invalid server metadata string encoding")
        if isinstance(value, list):
            pending.extend((item, depth + 1) for item in value)
        elif isinstance(value, dict):
            pending.extend((item, depth + 1) for pair in value.items() for item in pair)
    return result


def invalid_json():
    raise TaskError("invalid server metadata")


def sorted_numbers(values, maximum, empty=False):
    return (
        isinstance(values, list)
        and (empty or bool(values))
        and len(values) <= maximum
        and all(type(v) is int and 0 < v <= 0xFFFFFFFF for v in values)
        and all(a < b for a, b in zip(values, values[1:], strict=False))
    )


def validate_metadata(metadata, platform=None, build=None, version=None):
    if not isinstance(metadata, dict) or set(metadata) != METADATA_FIELDS:
        raise TaskError("server executable metadata has an unsupported shape")
    if any(
        not isinstance(metadata[field], str)
        for field in (
            "component",
            "build_identity",
            "platform",
            "version",
            "build_commit",
            "native_identifier",
        )
    ):
        raise TaskError("invalid server executable metadata scalar")
    identity = metadata["build_identity"]
    reported_version = metadata["version"]
    commit = metadata["build_commit"]
    storage = metadata["storage"]
    valid_version = isinstance(reported_version, str) and (
        bool(STABLE.fullmatch(reported_version))
        or bool(re.fullmatch(r"dev\.[0-9a-f]{12}(\.dirty)?", reported_version))
    )
    if (
        type(metadata["schema_version"]) is not int
        or metadata["schema_version"] != 2
        or metadata["component"] != "server"
        or identity not in BUILDS
        or metadata["platform"] not in PLATFORMS
        or not valid_version
        or not isinstance(commit, str)
        or not re.fullmatch(r"[0-9a-f]{40}", commit)
        or commit == "0" * 40
        or metadata["native_identifier"] != identifier(identity)
        or not sorted_numbers(metadata["protocol_majors"], 8)
        or any(v > 65535 for v in metadata["protocol_majors"])
        or not isinstance(storage, dict)
        or set(storage) != {"read", "write", "migrate_from"}
        or not sorted_numbers(storage["read"], 16)
        or not sorted_numbers(storage["migrate_from"], 16, empty=True)
        or type(storage["write"]) is not int
        or storage["write"] not in storage["read"]
        or (platform is not None and platform != metadata["platform"])
        or (build is not None and build != identity)
        or (version is not None and version != reported_version)
        or (identity == "release" and not STABLE.fullmatch(reported_version))
        or (
            reported_version.startswith("dev.")
            and reported_version.removesuffix(".dirty") != f"dev.{commit[:12]}"
        )
    ):
        raise TaskError("server executable metadata does not match the package")
    return metadata


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def entry_points(platform, build):
    if platform not in PLATFORMS:
        raise TaskError("unsupported server platform")
    identifier(build)
    return {
        "server": "bin/spaceterm-server"
        if platform == "linux-x86_64"
        else f"{bundle_name(build)}/Contents/MacOS/spaceterm-server",
        "cli": None,
    }


def validate_headless_name(name):
    if not relative_name(name):
        raise TaskError("server package contains an unsafe relative name")
    parts = PurePosixPath(name).parts
    if any(
        part.lower() in ("fonts", "icons", "sparkle.framework", "spaceterm.app")
        or any(
            term in part.lower()
            for term in (
                "spaceterm-server-launcher",
                "spaceterm-server-manager",
                "spaceterm-server-spawner",
                "spaceterm-native-",
                "systemd-creds",
                "credential-tools",
                "custody",
                "native-evidence",
                "native_evidence",
                "launchdaemon",
                "managementhelper",
                "authorizationrights",
            )
        )
        or part.lower().endswith((".desktop", ".icns", ".ttf", ".otf", ".dmg"))
        for part in parts
    ):
        raise TaskError("server package contains forbidden resources")


def validate_entitlements(entitlements):
    if entitlements != {}:
        raise TaskError("server runtime exception entitlements are not permitted")


def valid_sha256(value):
    return (
        isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)
    )
