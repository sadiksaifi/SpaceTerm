#!/usr/bin/env python
# MISE description="Tag HEAD for the release and write the plan every release job derives alike"
# USAGE arg "<version>"
# USAGE flag "--notes" help="Also write the git-cliff release notes to dist/release-notes.md"
import argparse

from spaceterm_tasks import main, release, release_plan


def run(version, notes):
    tag = release_plan.tag(version)
    plan = release_plan.create(tag)
    release_plan.PLAN.parent.mkdir(parents=True, exist_ok=True)
    release_plan.PLAN.write_text(release_plan.encode(plan))
    if notes:
        release.write_notes()
    print(f"Planned {tag} at {plan['source']}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument("--notes", action="store_true")
    args = parser.parse_args()
    main(lambda: run(args.version, args.notes))
