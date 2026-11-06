#!/usr/bin/env python
# MISE description="Run one release stage against this job's release plan"
# USAGE arg "<stage>" { choices "compile" "sign-core" "verify-core" "desktop" "verify-desktop" "floor-smoke" "resume" }
# USAGE arg "[archive]"
import argparse

from spaceterm_tasks import main, release_inputs, release_plan


def run(stage, archive):
    if stage == "compile":
        release_inputs.compile_products()
    elif stage == "sign-core":
        release_inputs.sign_core(archive)
    elif stage == "verify-core":
        release_inputs.verify_core()
    elif stage == "desktop":
        release_inputs.package_desktop(archive)
    elif stage == "verify-desktop":
        release_inputs.verify_desktop()
    elif stage == "resume":
        release_plan.resume_artifact(archive)
    else:
        from spaceterm_tasks.release_smoke import run

        run()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "stage",
        choices=(
            "compile",
            "sign-core",
            "verify-core",
            "desktop",
            "verify-desktop",
            "floor-smoke",
            "resume",
        ),
    )
    parser.add_argument("archive", nargs="?")
    args = parser.parse_args()
    main(lambda: run(args.stage, args.archive))
