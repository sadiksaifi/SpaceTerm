"""Run the release steps this host can run as a dry run in a disposable checkout.

The checkout lives in target/release-dry-run/source and holds HEAD of this checkout with a
dry-run tag, so no tag ever enters this repository. Each step runs as the release workflow
runs it, with a generated update key, ad hoc code signatures and no notarization. Nothing is
published. On a Mac the Release Core still compiles in the Team ID, so the caller supplies
SPACETERM_RELEASE_APPLE_TEAM_ID.
"""

import json
import os
import re
import subprocess

from spaceterm_tasks import ROOT, TaskError, macos_codesign, macos_signing, update_signing
from spaceterm_tasks.packaging import checked, require_clean_checkout
from spaceterm_tasks.release import create_dry_run_tag
from spaceterm_tasks.update_signing import DRY_RUN, SIGNING_SECRET, encoded_seed, generated_seed

CHECKOUT = ROOT / "target" / "release-dry-run" / "source"
DARWIN = "aarch64-apple-darwin"
LINUX = "x86_64-unknown-linux-gnu"


def git(*arguments, cwd=CHECKOUT):
    return checked(
        ["git", "-c", "protocol.file.allow=always", *arguments],
        "Git could not prepare the dry-run checkout",
        cwd=cwd,
        text=True,
    )


def configure_trust(seed, root):
    """Write public fixture trust into disposable source before any release compilation."""
    current = update_signing.derived_public_key(seed)
    backup = update_signing.derived_public_key(generated_seed())
    trust = {"current": current, "backup": backup}
    (root / update_signing.TRUST).write_text(json.dumps(trust, indent=2) + "\n")
    installer = root / "packaging/install.sh"
    text = installer.read_text()
    for name, public in (("CURRENT", current), ("BACKUP", backup)):
        text, count = re.subn(
            rf"^readonly UPDATE_KEY_{name}='[^'\n]*'$",
            lambda _: f"readonly UPDATE_KEY_{name}='{public}'",
            text,
            flags=re.M,
        )
        if count != 1:
            raise TaskError("the dry-run installer must declare each public update key once")
    installer.write_text(text)


def prepare_checkout(seed):
    """Check out HEAD with its submodules and this repository's tags, then tag it."""
    commit = git("rev-parse", "HEAD", cwd=ROOT).strip()
    if not (CHECKOUT / ".git").exists():
        CHECKOUT.parent.mkdir(parents=True, exist_ok=True)
        git("clone", "--quiet", "--shared", "--no-checkout", str(ROOT), str(CHECKOUT), cwd=ROOT)
    for tag in git("tag", "--list").split():
        git("tag", "--delete", tag)
    git("fetch", "--quiet", "--force", str(ROOT), "refs/tags/*:refs/tags/*")
    git("checkout", "--quiet", "--force", "--detach", commit)
    # Keep Cargo's build cache; remove everything else a previous dry run left.
    git("clean", "-ffdqx", "--exclude=/target/")
    modules = git("config", "--file", ".gitmodules", "--get-regexp", r"^submodule\..*\.path$")
    for line in modules.splitlines():
        key, path = line.split(maxsplit=1)
        name = key.removeprefix("submodule.").removesuffix(".path")
        git("config", f"submodule.{name}.url", str(ROOT / path))
    git("submodule", "update", "--quiet", "--init", "--recursive", "--force")
    configure_trust(seed, CHECKOUT)
    git("add", "packaging/update-trust.json", "packaging/install.sh")
    git(
        "-c",
        "user.name=SpaceTerm dry run",
        "-c",
        "user.email=dry-run@spaceterm.invalid",
        "-c",
        "commit.gpgSign=false",
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "--quiet",
        "--message",
        "test: compile disposable release trust",
    )
    return create_dry_run_tag(CHECKOUT)


def run(host):
    """Run every step this host builds; `host` is its Rust target."""
    require_clean_checkout()
    if host == DARWIN:
        macos_codesign.team_id(os.environ)
    seed = generated_seed()
    tag = prepare_checkout(seed)
    print(f"Dry run of {tag} at HEAD in target/release-dry-run/source")
    environment = macos_signing.signing_environment(os.environ)
    environment.pop("SPACETERM_RELEASE_UPDATE_TRUST_JSON", None)
    environment.update(
        {
            DRY_RUN: "1",
            SIGNING_SECRET: encoded_seed(seed),
            macos_codesign.SIGNING_IDENTITY: macos_codesign.ADHOC,
            "MISE_TRUSTED_CONFIG_PATHS": str(CHECKOUT),
            "SPACETERM_RELEASE_PLAN": "dist/release-plan.json",
        }
    )

    def task(*arguments):
        print(f"Dry run: mise run {' '.join(arguments)}", flush=True)
        if subprocess.run(["mise", "run", *arguments], cwd=CHECKOUT, env=environment).returncode:
            raise TaskError(f"the dry run failed at {arguments[0]}")

    task("release:plan", tag[1:], "--notes")
    task("release:core", tag)
    if host == DARWIN:
        # A Mac cross-builds the Linux Core so its desktop embeds both builds.
        task("release:core", tag, "--target", LINUX)
        task("release:desktop:macos", tag)
        print("Dry run: the Linux desktop builds only on x86_64 Linux; it is skipped.")
    else:
        print(
            "Dry run: the macOS Core and both desktops need a Mac; they are skipped. "
            "A release workflow run with publish off exercises both platforms."
        )
    task("release:assemble", tag, "--partial")
    print("Dry run finished: the assets are in target/release-dry-run/source/dist/release")
