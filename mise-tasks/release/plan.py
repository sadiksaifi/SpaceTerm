#!/usr/bin/env python
# MISE description="Verify release authority and freeze notes, floors and asset inventory"
# USAGE arg "<version>"
# USAGE arg "<mode>" { choices "unsigned" "signed" "publish" }
import argparse
import json
import os

from spaceterm_tasks import main, release, release_plan


def run(version, mode):
    tag = f"v{version}"
    release_plan.greenfield_tag(tag)
    import subprocess

    from spaceterm_tasks.packaging import checked

    if subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"], capture_output=True
    ).returncode:
        checked(
            [
                "tagsmith",
                "tag",
                "--target",
                "spaceterm",
                "--channel",
                "stable",
                "--version",
                version,
                "--json",
            ],
            "Tagsmith could not create the local release tag",
        )
    release.write_notes()
    plan = release_plan.create(tag, mode)
    release_plan.PLAN.write_text(json.dumps(plan, indent=2) + "\n")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a") as stream:
            stream.write(f"tag={tag}\nsource={plan['source']}\nmode={mode}\n")
    print(f"Planned {tag} at {plan['source']} ({mode})")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument("mode", choices=("unsigned", "signed", "publish"))
    args = parser.parse_args()
    main(lambda: run(args.version, args.mode))
