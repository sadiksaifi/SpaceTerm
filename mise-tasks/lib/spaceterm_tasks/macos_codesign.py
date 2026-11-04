"""Sign SpaceTerm's macOS code inside out under the hardened runtime, and verify it.

Release code carries Apple's Developer ID designated requirement for the Team ID the release
pipeline supplies. Every later build of the same identifier from that team satisfies it, so
privacy grants, Login Items and Keychain item access recorded for one version hold for the next.
Preflight and Development code is signed ad hoc with the same entitlements and runtime.

The release pipeline fills two variables. `SPACETERM_APPLE_TEAM_ID` is the ten-character Team
ID. `SPACETERM_APPLE_SIGNING_IDENTITY` is the certificate's name, "Developer ID Application:
<name> (<Team ID>)", or "-" for a dry run. codesign rejects code that does not satisfy its
own designated requirement, and an ad hoc signature cannot satisfy the Team ID requirement. A
dry run therefore signs ad hoc, checks that the Team ID requirement compiles, and verifies
everything else.

A dry run also signs without the hardened runtime. Under the runtime, library validation lets
a process load only code from its own Team ID, and ad hoc code has none, so a dry-run app
could not load Sparkle.framework. Every other build signs under the hardened runtime.
"""

import os
import plistlib
import re
import subprocess
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, sparkle
from spaceterm_tasks.packaging import checked
from spaceterm_tasks.update_signing import dry_run

TEAM_ID = "SPACETERM_APPLE_TEAM_ID"
SIGNING_IDENTITY = "SPACETERM_APPLE_SIGNING_IDENTITY"
ADHOC = "-"
CORE_ENTITLEMENTS = ROOT / "packaging/macos/core/Entitlements.plist"
# Apple's Developer ID markers: the intermediate (6.2.6) and leaf (6.1.13) certificate fields.
DEVELOPER_ID = (
    "anchor apple generic"
    " and certificate 1[field.1.2.840.113635.100.6.2.6]"
    " and certificate leaf[field.1.2.840.113635.100.6.1.13]"
)
HARDENED_RUNTIME = 0x10000
MACH_O_MAGIC = {
    bytes.fromhex(magic)
    for magic in (
        "feedface",
        "cefaedfe",
        "feedfacf",
        "cffaedfe",
        "cafebabe",
        "bebafeca",
        "cafebabf",
        "bfbafeca",
    )
}


def team_id(environment):
    value = environment.get(TEAM_ID, "")
    if not re.fullmatch(r"[0-9A-Z]{10}", value):
        raise TaskError(f"release signing requires a ten-character {TEAM_ID}")
    return value


def signing_identity(environment):
    """The team's Developer ID Application certificate name, or "-" for a dry run."""
    team = team_id(environment)
    value = environment.get(SIGNING_IDENTITY, "")
    if value == ADHOC or re.fullmatch(rf"Developer ID Application: [^\"\n]+ \({team}\)", value):
        return value
    raise TaskError(f"{SIGNING_IDENTITY} must name the team's Developer ID Application certificate")


def team_requirement(team):
    if not re.fullmatch(r"[0-9A-Z]{10}", team):
        raise TaskError("a Team ID has ten uppercase letters or digits")
    return f'{DEVELOPER_ID} and certificate leaf[subject.OU] = "{team}"'


def designated_requirement(identifier, team):
    return f'designated => identifier "{identifier}" and {team_requirement(team)}'


def sign(path, identity, *, identifier=None, entitlements=None, team=None, environment=None):
    """Sign one code object. Code nested in it must already carry its final signature."""
    arguments = ["codesign", "--force", "--sign", identity]
    if not dry_run(os.environ if environment is None else environment):
        arguments += ["--options", "runtime"]
    arguments.append("--timestamp=none" if identity == ADHOC else "--timestamp")
    if identifier is not None:
        arguments += ["--identifier", identifier]
    if entitlements is not None:
        arguments += ["--entitlements", str(entitlements)]
    if team is not None and identity != ADHOC:
        arguments += ["--requirements", f"={designated_requirement(identifier, team)}"]
    checked([*arguments, str(path)], f"code signing failed for {Path(path).name}", env=environment)


def sign_core(bundle, identity, identifier, *, team=None, environment=None):
    sign(
        bundle,
        identity,
        identifier=identifier,
        entitlements=CORE_ENTITLEMENTS,
        team=team,
        environment=environment,
    )


def sign_desktop(app, identity, entitlements, *, team=None, environment=None):
    """Sign Sparkle's code, then the app with its identity's entitlements."""
    contents = Path(app) / "Contents"
    identifier = plistlib.loads((contents / "Info.plist").read_bytes())["CFBundleIdentifier"]
    frameworks = contents / "Frameworks"
    if frameworks.exists():
        if {path.name for path in frameworks.iterdir()} != {"Sparkle.framework"}:
            raise TaskError("the app may embed only Sparkle.framework")
        sparkle.sign_framework(
            frameworks / "Sparkle.framework",
            lambda path: sign(path, identity, environment=environment),
        )
    sign(
        app,
        identity,
        identifier=identifier,
        entitlements=entitlements,
        team=team,
        environment=environment,
    )


def sign_disk_image(dmg, identity, identifier, *, environment=None):
    if identity == ADHOC:
        return
    checked(
        [
            "codesign",
            "--force",
            "--sign",
            identity,
            "--identifier",
            identifier,
            "--timestamp",
            str(dmg),
        ],
        "disk image signing failed",
        env=environment,
    )


def canonical_requirement(text):
    """The text codesign prints for a compiled requirement."""
    return checked(
        ["csreq", f"-r={text}", "-t"], "the designated requirement does not compile", text=True
    ).strip()


def details(path):
    result = subprocess.run(
        ["codesign", "--display", "--verbose=4", str(path)], capture_output=True, text=True
    )
    if result.returncode:
        raise TaskError(f"{Path(path).name} has no readable signature")
    return result.stderr


def signing_identifier(path):
    for line in details(path).splitlines():
        if line.startswith("Identifier="):
            return line.removeprefix("Identifier=")
    raise TaskError(f"{Path(path).name} has no signing identifier")


def verify_disk_image(dmg, identifier, team):
    requirement = f'=identifier "{identifier}" and {team_requirement(team)}'
    checked(
        ["codesign", "--verify", "--strict", "-R", requirement, str(dmg)],
        "the disk image is not signed with the team's Developer ID certificate",
    )


def signed_entitlements(path):
    value = checked(
        ["codesign", "--display", "--entitlements", ":-", str(path)],
        f"{Path(path).name} entitlements could not be read",
    )
    return plistlib.loads(value) if value.strip() else {}


def verify(path, identifier, expected_entitlements, *, team=None, identity=ADHOC, label="code"):
    """Require a strict seal, hardened runtime, exact entitlements and the identity's requirement.

    A dry run's code carries no hardened runtime, so its absence passes there.

    A Developer ID signature must carry the team's designated requirement and satisfy it, which
    needs Apple's certificate chain, and so must every native file in a bundle. An ad hoc
    signature with a team only proves that the team's requirement compiles.
    """
    path = Path(path)
    checked(
        ["codesign", "--verify", "--strict", "--deep", str(path)],
        f"{label} signature verification failed",
    )
    checked(
        ["codesign", "--verify", "--strict", "-R", f'=identifier "{identifier}"', str(path)],
        f"{label} signing identifier differs from {identifier}",
    )
    detail = details(path)
    flags = re.search(r"^CodeDirectory .*flags=0x([0-9a-f]+)", detail, re.MULTILINE)
    if not flags:
        raise TaskError(f"{label} has no code directory flags")
    if not dry_run() and not int(flags.group(1), 16) & HARDENED_RUNTIME:
        raise TaskError(f"{label} is not signed with the hardened runtime")
    if signed_entitlements(path) != expected_entitlements:
        raise TaskError(f"{label} entitlements differ from its identity's")
    if identity == ADHOC:
        if "Signature=adhoc" not in detail.splitlines():
            raise TaskError(f"{label} is not ad hoc signed")
        if team is not None:
            canonical_requirement(designated_requirement(identifier, team))
        return
    if team is None:
        raise TaskError(f"{label} needs a Team ID to verify a Developer ID signature")
    designated = checked(
        ["codesign", "--display", "--requirements", "-", str(path)],
        f"{label} designated requirement could not be read",
        text=True,
    ).strip()
    if designated != canonical_requirement(designated_requirement(identifier, team)):
        raise TaskError(f"{label} designated requirement is not the team's")
    if f"TeamIdentifier={team}" not in detail.splitlines():
        raise TaskError(f"{label} is not signed by team {team}")
    # Notarization requires a secure timestamp; codesign shows it as Timestamp, not Signed Time.
    if not any(line.startswith("Timestamp=") for line in detail.splitlines()):
        raise TaskError(f"{label} has no secure timestamp")
    requirement = f"={team_requirement(team)}"
    checked(
        ["codesign", "--verify", "--strict", "--deep", "-R", requirement, str(path)],
        f"{label} is not signed with the team's Developer ID certificate",
    )
    if path.is_dir():
        for nested in path.rglob("*"):
            if nested.is_symlink() or not nested.is_file():
                continue
            with nested.open("rb") as binary:
                native = binary.read(4) in MACH_O_MAGIC
            if native:
                checked(
                    ["codesign", "--verify", "--strict", "-R", requirement, str(nested)],
                    f"{label} contains native code without the team's signature",
                )
