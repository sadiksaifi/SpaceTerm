"""Release asset names and the signed JSON feeds of SpaceTerm Core and the Linux desktop.

A feed is compact UTF-8 JSON of at most 4096 bytes beside a `.sig` of 64 raw Ed25519 bytes over
the feed's exact bytes. Readers verify that signature before they parse any field, then refuse
unknown fields and every value that breaks a field rule. `check` applies the same rules to
what this module writes, so the writer cannot drift from the readers.
"""

import base64
import json
import re
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.update_signing import SIGNATURE_BYTES, sha256, sign, verify

RELEASES = "https://github.com/sadiksaifi/SpaceTerm/releases"
SCHEMA = 1
MAX_FEED = 4096
MAX_ARCHIVE = 512 * 1024 * 1024
APPCAST = "appcast.xml"
CHECKSUMS = "SHA256SUMS"
INSTALLER = "install.sh"
PLATFORMS = {"aarch64-apple-darwin": "macos-arm64", "x86_64-unknown-linux-gnu": "linux-x86_64"}
# The oldest host each platform's archives run on: macOS, then glibc.
MINIMUM_SYSTEM = {"macos-arm64": "26.0", "linux-x86_64": "2.35"}
# Every product and platform with a JSON feed. The macOS desktop has the Sparkle appcast.
FEEDS = (("core", "macos-arm64"), ("core", "linux-x86_64"), ("desktop", "linux-x86_64"))
PROTOCOLS = ("work", "service", "desktop_channel")
# The file that owns this build's protocol versions.
PROTOCOL_SOURCE = Path("crates/spaceterm-core/src/protocol/version.rs")
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
FEED_KEYS = {
    "schema",
    "product",
    "platform",
    "version",
    "floor",
    "published_at",
    "protocols",
    "minimum_system",
    "notes",
    "archive",
}
ARCHIVE_KEYS = {"name", "size", "sha256", "signature"}


def feed_name(product, platform):
    return f"spaceterm-{product}-{platform}.json"


def archive_name(product, version, platform):
    return f"spaceterm-{product}-{version}-{platform}.tar.gz"


def dmg_name(version):
    return f"SpaceTerm-{version}-macos-arm64.dmg"


def notes_url(version):
    return f"{RELEASES}/tag/v{version}"


def download_url(version, name):
    return f"{RELEASES}/download/v{version}/{name}"


def asset_names(version):
    """Every release asset, SHA256SUMS last because it lists all the others."""
    names = [dmg_name(version), APPCAST]
    for product, platform in FEEDS:
        archive = archive_name(product, version, platform)
        feed = feed_name(product, platform)
        names += [archive, f"{archive}.sig", feed, f"{feed}.sig"]
    return [*names, INSTALLER, CHECKSUMS]


def protocols(root=ROOT):
    """This build's protocol versions, as `protocol/version.rs` declares them."""
    try:
        source = (root / PROTOCOL_SOURCE).read_text()
    except OSError:
        raise TaskError("the protocol version source is unavailable") from None
    result = {}
    for name in PROTOCOLS:
        found = re.findall(
            rf"^pub const {name.upper()}: ProtocolVersion = "
            r"ProtocolVersion \{ major: ([0-9]+), minor: ([0-9]+) \};$",
            source,
            re.MULTILINE,
        )
        if len(found) != 1:
            raise TaskError(f"the protocol version source must declare {name.upper()} once")
        major, minor = (int(part) for part in found[0])
        if not (major <= 65535 and minor <= 65535):
            raise TaskError("a protocol version does not fit in 16 bits")
        result[name] = {"major": major, "minor": minor}
    return result


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def _order(version):
    return tuple(int(part) for part in version.split("."))


def _u16(value):
    return type(value) is int and 0 <= value <= 65535


def check(data, product, platform):
    """Apply every reader rule to feed bytes and return the document."""
    try:
        if len(data) > MAX_FEED:
            raise ValueError("the feed is too large")
        document = json.loads(data.decode("utf-8"), object_pairs_hook=_unique)
        if not isinstance(document, dict) or set(document) != FEED_KEYS:
            raise ValueError("the feed fields differ from schema 1")
        version = document["version"]
        archive = document["archive"]
        listed = document["protocols"]
        if (
            type(document["schema"]) is not int
            or document["schema"] != SCHEMA
            or (document["product"], document["platform"]) != (product, platform)
            or (product, platform) not in FEEDS
            or not isinstance(version, str)
            or len(version) > 64
            or not VERSION.fullmatch(version)
            or not isinstance(document["floor"], str)
            or not VERSION.fullmatch(document["floor"])
            or _order(document["floor"]) > _order(version)
            or type(document["published_at"]) is not int
            or document["published_at"] <= 0
            or not isinstance(listed, dict)
            or set(listed) != set(PROTOCOLS)
            or any(
                not isinstance(listed[name], dict)
                or set(listed[name]) != {"major", "minor"}
                or not all(_u16(part) for part in listed[name].values())
                for name in PROTOCOLS
            )
            or document["minimum_system"] != MINIMUM_SYSTEM[platform]
            or document["notes"] != notes_url(version)
            or not isinstance(archive, dict)
            or set(archive) != ARCHIVE_KEYS
            or archive["name"] != archive_name(product, version, platform)
            or type(archive["size"]) is not int
            or not 0 < archive["size"] <= MAX_ARCHIVE
            or not isinstance(archive["sha256"], str)
            or not re.fullmatch(r"[0-9a-f]{64}", archive["sha256"])
            or not isinstance(archive["signature"], str)
            or len(archive["signature"]) != 88
            or len(base64.b64decode(archive["signature"], validate=True)) != SIGNATURE_BYTES
        ):
            raise ValueError("a feed field breaks its rule")
        if json.dumps(document, separators=(",", ":"), ensure_ascii=False).encode() != data:
            raise ValueError("the feed is not compact JSON")
    except (ValueError, UnicodeError):
        raise TaskError(
            f"the {feed_name(product, platform)} feed breaks the feed contract"
        ) from None
    return document


def write(
    seed, keys, directory, *, product, platform, version, floor, published_at, protocols, archive
):
    """Write `<feed>.json` and its raw signature for `archive`, whose `.sig` lies beside it.

    `floor` is the product's Update Floor, the oldest version that may update to `version`.

    The archive signature must verify under `keys` first. The feed signature is verified after
    signing, so a written feed always verifies under the same trust its readers hold.
    """
    archive = Path(archive)
    signature = archive.with_name(f"{archive.name}.sig")
    if archive.name != archive_name(product, version, platform):
        raise TaskError("the feed archive name differs from the release asset name")
    verify(keys, archive, signature)
    document = {
        "schema": SCHEMA,
        "product": product,
        "platform": platform,
        "version": version,
        "floor": floor,
        "published_at": published_at,
        "protocols": protocols,
        "minimum_system": MINIMUM_SYSTEM[platform],
        "notes": notes_url(version),
        "archive": {
            "name": archive.name,
            "size": archive.stat().st_size,
            "sha256": sha256(archive),
            "signature": base64.b64encode(signature.read_bytes()).decode(),
        },
    }
    data = json.dumps(document, separators=(",", ":"), ensure_ascii=False).encode()
    check(data, product, platform)
    feed = Path(directory) / feed_name(product, platform)
    feed.write_bytes(data)
    feed_signature = feed.with_name(f"{feed.name}.sig")
    sign(seed, feed, feed_signature)
    verify(keys, feed, feed_signature)
    return feed, feed_signature
