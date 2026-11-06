"""Package planned release inputs, sign updater feeds and publish complete immutable drafts.

The manual release workflow plans notes and floors, validates alongside compilation, signs
Core, packages the compiled desktops, exercises the final Core and assembles updater assets.
Publication holds Latest until the separate promotion workflow runs. Local packaging tasks
retain their compile-and-package interface. A local compiling dry run uses disposable public
trust, ad hoc signatures and no notarization or publication.
"""

import base64
import json
import os
import plistlib
import re
import shutil
import subprocess
import tempfile
import tomllib
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

from spaceterm_tasks import (
    ROOT,
    TaskError,
    macos_signing,
    release_feed,
    release_floor,
    sparkle,
    update_signing,
)
from spaceterm_tasks.packaging import DIST, STABLE_TAG, checked, release_version
from spaceterm_tasks.release_feed import (
    APPCAST,
    CHECKSUMS,
    INSTALLER,
    archive_name,
    asset_names,
    dmg_name,
    download_url,
    notes_url,
)

SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
RELEASES = release_feed.RELEASES
INSTALLER_SOURCE = ROOT / "packaging" / "install.sh"
# The directory each step writes, which the next steps read.
CORE = DIST / "release-core"
NOTES = DIST / "release-notes.md"
DESKTOP_MACOS = DIST / "release-desktop-macos"
DESKTOP_LINUX = DIST / "release-desktop-linux"
RELEASE = DIST / "release"
CORE_FILES = ("artifact.tar.gz", "artifact.sig", "artifact.sha256")


def sha256(path):
    return update_signing.sha256(path)


def checksums(paths):
    return "".join(f"{sha256(path)}  {Path(path).name}\n" for path in paths)


def replace_directory(staged, destination):
    shutil.rmtree(destination, ignore_errors=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    staged.rename(destination)


def notes(root=ROOT):
    """Markdown notes for the release tagged at HEAD, from its Conventional Commits.

    cliff.toml's tag_pattern decides which tags are releases.
    """
    with open(root / "cliff.toml", "rb") as config:
        release_tag = re.compile(tomllib.load(config)["git"]["tag_pattern"])
    tags = subprocess.run(
        ["git", "tag", "--points-at", "HEAD"], cwd=root, capture_output=True, text=True
    ).stdout.split()
    if not any(release_tag.match(tag) for tag in tags):
        raise TaskError("release notes require a stable release tag from 0.10.0 at HEAD")
    result = subprocess.run(
        ["git-cliff", "--config", "cliff.toml", "--current", "--offline", "--strip", "all"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    if result.returncode or not result.stdout.strip():
        raise TaskError("release notes require a stable release tag at HEAD")
    return result.stdout


def write_notes(output=NOTES, root=ROOT):
    """Write the notes file that the desktop builds embed and the GitHub release shows."""
    text = notes(root)
    if len(text.encode()) > 64 * 1024:
        raise TaskError("the release notes are larger than the 64 KiB a desktop embeds")
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text)
    print(f"Wrote the release notes to {output.name}")
    return output


def dry_run_version(tags):
    """A version newer than every stable tag, from 0.10.0, so a dry run never reuses a tag."""
    versions = [
        tuple(int(part) for part in match.groups())
        for match in (STABLE_TAG.fullmatch(tag) for tag in tags)
        if match
    ]
    newest = max(versions, default=(0, 0, 0))
    if newest < (0, 10, 0):
        return "0.10.0"
    return f"{newest[0]}.{newest[1]}.{newest[2] + 1}"


def dry_run_tag(root=ROOT):
    tags = checked(["git", "tag", "--list"], "Git could not list tags", cwd=root, text=True)
    return f"v{dry_run_version(tags.split())}"


def create_dry_run_tag(root=ROOT):
    """Tag HEAD for a dry run in a disposable checkout, and return the tag."""
    tag = dry_run_tag(root)
    checked(
        [
            "git",
            "-c",
            "user.name=SpaceTerm dry run",
            "-c",
            "user.email=dry-run@spaceterm.invalid",
            "tag",
            "--annotate",
            "--no-sign",
            tag,
            "--message",
            f"Dry run of SpaceTerm {tag[1:]}",
        ],
        "Git could not tag the dry run",
        cwd=root,
    )
    return tag


def core(tag, target=None):
    """Build and sign SpaceTerm Core for `target`, the host by default, into dist/release-core."""
    from spaceterm_tasks.core_package import host_target, package

    target = target or host_target()
    archive, signature = package("spaceterm", tag, target=target)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-core-") as temporary:
        staged = Path(temporary) / target
        staged.mkdir()
        shutil.copyfile(archive, staged / "artifact.tar.gz")
        shutil.copyfile(signature, staged / "artifact.sig")
        (staged / "artifact.sha256").write_text(sha256(archive) + "\n")
        replace_directory(staged, CORE / target)
    print(f"SpaceTerm Core for {target} is signed and ready to embed")


def core_catalog(directory):
    """Require dist/release-core to hold both targets' signed Core builds."""
    from spaceterm_tasks.core_package import TARGETS

    directory = Path(directory)
    for target in TARGETS.values():
        if not all((directory / target / name).is_file() for name in CORE_FILES):
            raise TaskError(f"the release Core builds lack {target}; run release:core on it")
    return directory


def desktop_macos(tag, notes_file=NOTES, core_directory=CORE):
    """Package the macOS desktop with both Core builds, then write its signed appcast."""
    from spaceterm_tasks import package_macos

    os.environ["SPACETERM_CLI_ARTIFACTS"] = str(core_catalog(core_directory).resolve())
    os.environ.pop("SPACETERM_CLI_ARCHIVE", None)
    os.environ.pop("SPACETERM_CLI_SIGNATURE", None)
    package_macos.package(tag, notes_file)
    appcast(tag)


def desktop_linux(tag, notes_file=NOTES, core_directory=CORE):
    """Package the Linux desktop with both Core builds."""
    from spaceterm_tasks import package_linux

    os.environ["SPACETERM_CLI_ARTIFACTS"] = str(core_catalog(core_directory).resolve())
    os.environ.pop("SPACETERM_CLI_ARCHIVE", None)
    os.environ.pop("SPACETERM_CLI_SIGNATURE", None)
    package_linux.package(tag, notes_file)
    version = release_version(tag)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-desktop-") as temporary:
        staged = Path(temporary) / "desktop"
        staged.mkdir()
        shutil.copyfile(
            package_linux.archive_path("spaceterm"),
            staged / archive_name("desktop", version, "linux-x86_64"),
        )
        replace_directory(staged, DESKTOP_LINUX)
    print("The Linux desktop archive is ready to sign")


def sparkle_signing(environment, plist):
    """Sparkle's options for the update key, which must be the key the packaged app trusts.

    A release app pins the current key of update-trust.json, a dry-run app its generated key.
    """
    seed = update_signing.release_seed(environment)
    if plist.get("SUPublicEDKey") != update_signing.trust(environment)[0]:
        raise TaskError("the app's SUPublicEDKey must be the current update key")
    return ["--ed-key-file", "-"], update_signing.encoded_seed(seed).encode()


def _item_version(item, name):
    return item.findtext(f"{{{SPARKLE_NS}}}{name}")


def _order(version):
    return tuple(int(part) for part in version.split("."))


def _step(item, version):
    """Check one appcast item of release `version` and return its floor and disk image
    signature."""
    floor = _item_version(item, "minimumUpdateVersion")
    if (
        _item_version(item, "version") != version
        or _item_version(item, "shortVersionString") != version
    ):
        raise ValueError("an appcast version differs from the release it encloses")
    if not floor or not release_feed.VERSION.fullmatch(floor) or _order(floor) > _order(version):
        raise ValueError("each appcast item names an Update Floor at or below its version")
    if item.findtext(f"{{{SPARKLE_NS}}}fullReleaseNotesLink") != notes_url(version):
        raise ValueError("the appcast must link the release notes")
    if item.find(f"{{{SPARKLE_NS}}}channel") is not None:
        raise ValueError("release channels are not supported")
    enclosure = item.find("enclosure")
    if enclosure is None:
        raise ValueError("the release disk image is missing")
    if enclosure.get("url") != download_url(version, dmg_name(version)):
        raise ValueError("the release disk image URL does not match")
    signature = enclosure.get(f"{{{SPARKLE_NS}}}edSignature", "")
    if len(base64.b64decode(signature, validate=True)) != 64:
        raise ValueError("the release disk image signature is missing")
    return floor, signature, enclosure


def verify_appcast(path, archive, version, floor):
    """Require the appcast of release `version` with Update Floor `floor`: one item per step.

    The first item encloses `archive` and names `floor` as `sparkle:minimumUpdateVersion`.
    Every item whose floor is below its version is followed by its floor release's item, so
    the chain ends at the first release, which is its own floor. Sparkle installs the newest
    item whose floor the installed version meets. Returns the disk image's signature.
    """
    items = ET.parse(path).getroot().findall("./channel/item")
    if not items:
        raise ValueError("the appcast must contain the release")
    named, signature, enclosure = _step(items[0], version)
    if named != floor:
        raise ValueError("the appcast must name the release's Update Floor")
    if items[0].findtext(f"{{{SPARKLE_NS}}}minimumSystemVersion") != "26.0":
        raise ValueError("the appcast minimum system version must be 26.0")
    if enclosure.get("url") != download_url(version, archive.name) or enclosure.get(
        "length"
    ) != str(archive.stat().st_size):
        raise ValueError("the release disk image URL or size does not match")
    step = named
    for index, item in enumerate(items[1:], start=1):
        if step == _item_version(items[index - 1], "version"):
            raise ValueError("the appcast goes on past a release that is its own floor")
        step = _step(item, step)[0]
    if step != _item_version(items[-1], "version"):
        raise ValueError("the appcast lacks its floor release's items")
    return signature


# The signature comment Sparkle's sign_update appends to a feed. It signs the bytes before it.
SPARKLE_SIGNATURE = re.compile(
    rb"<!-- sparkle-signatures:\nedSignature: ([A-Za-z0-9+/=]{88})\nlength: ([0-9]+)\n-->\n?\Z"
)
MAX_APPCAST = 1024 * 1024


def authenticated_appcast(data, keys):
    """Require a Sparkle feed signature over `data` under one of `keys`."""
    found = SPARKLE_SIGNATURE.search(data)
    if found is None:
        raise TaskError("the floor release's appcast carries no Sparkle signature")
    with tempfile.TemporaryDirectory(prefix="spaceterm-appcast-") as temporary:
        message, signature = Path(temporary) / "appcast", Path(temporary) / "appcast.sig"
        message.write_bytes(data[: found.start()])
        signature.write_bytes(base64.b64decode(found.group(1), validate=True))
        update_signing.verify(keys, message, signature)
    return data


def _download(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read(MAX_APPCAST + 1)


def floor_appcast(floor, version, trust=update_signing.release_trust, fetch=_download):
    """The signed appcast of the floor release, which the new appcast continues; None when
    the release is its own floor. A published release signed it under `trust()`, the release
    trust, even in a dry run."""
    if floor == version:
        return None
    try:
        data = fetch(download_url(floor, APPCAST))
    except OSError:
        raise TaskError("the floor release's appcast could not be downloaded") from None
    if len(data) > MAX_APPCAST:
        raise TaskError("the floor release's appcast is too large")
    return authenticated_appcast(data, trust())


def write_appcast(directory, version, floor, previous, run):
    """Write and sign the appcast of the disk image of `version` in `directory`.

    `previous` is the floor release's signed appcast, or None when the release is its own
    floor. generate_appcast adds the new item above every item of `previous` and keeps them
    all, then sign_update signs the result. `run(tool, *arguments)` runs a Sparkle tool with
    the update key.
    """
    directory = Path(directory)
    archive = directory / dmg_name(version)
    feed = directory / APPCAST
    if previous is not None:
        feed.write_bytes(previous)
    run(
        sparkle.tool("generate_appcast"),
        "--maximum-deltas",
        "0",
        "--maximum-versions",
        "0",
        "--minimum-update-version",
        floor,
        "--download-url-prefix",
        f"{RELEASES}/download/v{version}/",
        "--full-release-notes-url",
        notes_url(version),
        "--link",
        notes_url(version),
        directory,
    )
    try:
        signature = verify_appcast(feed, archive, version, floor)
    except (OSError, ValueError, ET.ParseError) as error:
        raise TaskError(str(error)) from None
    run(sparkle.tool("sign_update"), feed)
    run(sparkle.tool("sign_update"), "--verify", archive, signature)
    run(sparkle.tool("sign_update"), "--verify", feed)
    return feed


def appcast(tag):
    """Sign the packaged disk image into dist/release-desktop-macos with its Sparkle appcast."""
    version = release_version(tag)
    plist = plistlib.loads((DIST / "SpaceTerm.app/Contents/Info.plist").read_bytes())
    if plist["CFBundleShortVersionString"] != version or plist["CFBundleVersion"] != version:
        raise TaskError("package the selected Git tag before writing its appcast")
    options, stdin = sparkle_signing(os.environ, plist)
    environment = macos_signing.signing_environment(os.environ)
    floor = release_floor.floor("desktop", version)
    previous = floor_appcast(floor, version)

    def run(tool, *arguments):
        if subprocess.run(
            [tool, *options, *arguments], input=stdin, env=environment, capture_output=True
        ).returncode:
            raise TaskError("Sparkle could not sign or verify the appcast")

    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-appcast-") as temporary:
        directory = Path(temporary) / "desktop"
        directory.mkdir()
        shutil.copyfile(DIST / "SpaceTerm.dmg", directory / dmg_name(version))
        write_appcast(directory, version, floor, previous, run)
        replace_directory(directory, DESKTOP_MACOS)
    print(f"The disk image and its signed appcast with Update Floor {floor} are verified")


def verified_core(entry, target, version, floor, directory):
    """Authenticate one Core build of `version` with Update Floor `floor` and return its archive
    and signature."""
    from spaceterm_tasks.core_package import document, host_target, verified_copy

    if (entry / "artifact.sha256").read_text() != sha256(entry / "artifact.tar.gz") + "\n":
        raise TaskError(f"the {target} Core build differs from its digest")
    tree, archive, signature = verified_copy(
        entry / "artifact.tar.gz",
        entry / "artifact.sig",
        directory,
        None,
        identity="spaceterm",
        target=target,
        native=target == host_target(),
    )
    info = document((tree / "build.json").read_bytes())
    if os.environ.get("SPACETERM_RELEASE_PLAN"):
        from spaceterm_tasks.release_plan import load

        if info["commit"] != load()["source"]:
            raise TaskError("the Core source differs from the release plan")
    if info["version"] != version:
        raise TaskError("the Core version must match the release tag")
    if info["floor"] != floor:
        raise TaskError("the Core build's Update Floor differs from the release's")
    return archive, signature


def assemble(
    tag,
    core_directory=CORE,
    macos_directory=DESKTOP_MACOS,
    linux_directory=DESKTOP_LINUX,
    *,
    complete=True,
    environment=os.environ,
):
    """Sign every feed and the Linux desktop archive and write dist/release with SHA256SUMS.

    A release requires every asset. A local dry run, which cannot build both desktops on one
    host, omits the products its host does not build.
    """
    from spaceterm_tasks.core_package import PLATFORMS

    version = release_version(tag)
    from spaceterm_tasks import release_plan

    plan = (
        release_plan.load(environment["SPACETERM_RELEASE_PLAN"], root=ROOT)
        if environment.get("SPACETERM_RELEASE_PLAN")
        else None
    )
    if plan is not None and plan["tag"] != tag:
        raise TaskError("assembly requires the planned release tag")
    if not complete and not update_signing.dry_run(environment):
        raise TaskError("only a dry run may assemble a release without every asset")
    if complete and environment.get("SPACETERM_RELEASE_PLAN"):
        from spaceterm_tasks.release_inputs import verify_evidence

        verify_evidence()
    seed = update_signing.release_seed(environment)
    keys = update_signing.trust(environment)
    protocols = release_feed.protocols()
    floors = plan["floors"] if plan is not None else release_floor.floors(version)
    published_at = plan["published_at"] if plan is not None else release_plan.publication_time(tag)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-") as temporary:
        directory = Path(temporary) / "release"
        directory.mkdir()
        scratch = Path(temporary) / "scratch"
        scratch.mkdir()
        archives = []
        for target, platform in PLATFORMS.items():
            entry = Path(core_directory) / target
            if not entry.is_dir() and not complete:
                print(f"Dry run: no {platform} Core build on this host; its assets are skipped.")
                continue
            verified = scratch / target
            verified.mkdir()
            archive, signature = verified_core(entry, target, version, floors["core"], verified)
            name = archive_name("core", version, platform)
            shutil.copyfile(archive, directory / name)
            shutil.copyfile(signature, directory / f"{name}.sig")
            archives.append(("core", platform, directory / name))
        linux = Path(linux_directory) / archive_name("desktop", version, "linux-x86_64")
        if linux.is_file() or complete:
            from spaceterm_tasks import package_linux

            package_linux.verify(linux, tag)
            shutil.copyfile(linux, directory / linux.name)
            update_signing.sign(seed, directory / linux.name, directory / f"{linux.name}.sig")
            archives.append(("desktop", "linux-x86_64", directory / linux.name))
        else:
            print("Dry run: no Linux desktop build on this host; its assets are skipped.")
        for product, platform, archive in archives:
            release_feed.write(
                seed,
                keys,
                directory,
                product=product,
                platform=platform,
                version=version,
                floor=floors[product],
                published_at=published_at,
                protocols=protocols,
                archive=archive,
            )
        dmg = Path(macos_directory) / dmg_name(version)
        if dmg.is_file() or complete:
            for name in (dmg.name, APPCAST):
                shutil.copyfile(Path(macos_directory) / name, directory / name)
            try:
                verify_appcast(
                    directory / APPCAST, directory / dmg.name, version, floors["desktop"]
                )
            except (OSError, ValueError, ET.ParseError) as error:
                raise TaskError(str(error)) from None
        else:
            print("Dry run: no macOS desktop build on this host; its assets are skipped.")
        installer(directory / INSTALLER, environment, version=version)
        listed = sorted(path for path in directory.iterdir())
        (directory / CHECKSUMS).write_text(checksums(listed))
        if complete:
            require_assets(directory, version)
        replace_directory(directory, RELEASE)
    print(f"Assembled {len(listed) + 1} release assets with checksums")


def installer(destination, environment=os.environ, source=INSTALLER_SOURCE, root=ROOT, *, version):
    """Pin the Linux installer to its assembled release and signing trust.

    Check the source keys against tracked trust first. Dry runs replace them with their
    generated key, so they exercise signature verification without trusting release keys.
    """
    if not re.fullmatch(r"(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})", version):
        raise TaskError("the installer requires a stable release version")
    text = Path(source).read_text()
    for token in ("@RELEASE_KIND@", "@DEFAULT_VERSION@"):
        if text.count(token) != 1:
            raise TaskError(f"packaging/install.sh must hold {token} exactly once")
    pinned = dict(re.findall(r"^readonly UPDATE_KEY_(CURRENT|BACKUP)='([^'\n]*)'$", text, re.M))
    try:
        trust = json.loads((root / update_signing.TRUST).read_text())
    except (OSError, ValueError):
        raise TaskError("packaging/update-trust.json is missing or malformed") from None
    if pinned != {"CURRENT": trust.get("current"), "BACKUP": trust.get("backup")}:
        raise TaskError("packaging/install.sh must pin the keys in packaging/update-trust.json")
    if update_signing.dry_run(environment):
        kind = "dry-run"
        key = update_signing.trust(environment, root)[0]
        for pinned_key in pinned.values():
            text = text.replace(pinned_key, key)
    else:
        update_signing.release_trust(root)
        kind = "release"
    Path(destination).write_text(
        text.replace("@RELEASE_KIND@", kind).replace("@DEFAULT_VERSION@", version)
    )
    Path(destination).chmod(0o755)


def require_assets(directory, version):
    """Require exactly the release assets, each listed once with its digest in SHA256SUMS."""
    names = asset_names(version)
    if sorted(path.name for path in Path(directory).iterdir()) != sorted(names):
        raise TaskError("the release assets differ from the release asset list")
    listed = [path for path in (Path(directory) / name for name in names) if path.name != CHECKSUMS]
    if (Path(directory) / CHECKSUMS).read_text() != checksums(sorted(listed)):
        raise TaskError("the release assets differ from their checksums")


def publish(tag, notes_file=NOTES, directory=RELEASE, environment=os.environ):
    """Publish the assembled release with the notes the desktops embed; a published release is
    never replaced."""
    version = release_version(tag)
    require_assets(directory, version)
    head = checked(["git", "rev-parse", "HEAD"], "Git could not read HEAD", cwd=ROOT).strip()
    tagged = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
    )
    if tagged.returncode or tagged.stdout.strip() != head:
        raise TaskError("check out the release tag before publishing")
    if not Path(notes_file).is_file():
        raise TaskError("publishing needs the release notes from release:notes")
    assets = [Path(directory) / name for name in asset_names(version)]
    if update_signing.dry_run(environment):
        print(f"Dry run: verified {len(assets)} release assets for {tag}; nothing is published.")
        return
    from spaceterm_tasks import release_plan

    plan = release_plan.load()
    if plan["tag"] != tag:
        raise TaskError("publication requires the planned release tag")
    release_plan.recheck(plan)
    from spaceterm_tasks.release_publish import publish_draft

    publish_draft(plan, assets, notes_file)
