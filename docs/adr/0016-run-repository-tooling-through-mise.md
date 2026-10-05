# Run repository tooling through mise

mise is the only developer toolchain and task runner. `.mise.toml` pins every tool, and `mise.lock` records each tool's checksums, including the Sparkle distribution that release builds link. A second Python toolchain or task runner would split the source of truth for versions, so the repository uses neither.

Tasks that run one external tool are defined in `.mise.toml`. Tasks with logic are Python file tasks in `mise-tasks/`, named by their path, and share code through `mise-tasks/lib`. Task code uses the standard library and is tested with `unittest`, so a pinned Python is its only requirement. The fonts tasks are the exception: they install hash-pinned `fonttools` into a private environment. Python replaced the shell scripts because the same task must run on macOS, Linux, and later Windows. The release installer stays POSIX shell because users pipe it into `sh`.

Tooling prefers maintained external tools to repository code: `cargo-packager` builds the app and disk image, lefthook and cocogitto validate commit messages, Tagsmith validates release tags, git-cliff writes release notes, `gh release` publishes releases, `brew bump-cask-pr` updates the Homebrew cask, `cargo-sweep` removes stale build artifacts, and `mise doctor project` and `mise bootstrap packages` check and prepare a host.

A task name without a platform segment works on every supported platform. When platforms need different implementations, the generic task dispatches to `<task>:{{ os() }}`, and each implementation carries its platform as the last segment. A task that exists on one platform only carries that platform segment and has no generic name.

`build.rs` is pure Rust so that a Cargo build needs no other language. It derives the build identity from Git and enforces the release tag rules of ADR 0012.
