use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

// The workspace gitlink owns the Ghostty revision. Cargo builds a patched copy of that
// checkout, never fetching a different source revision or modifying the submodule.
const BUILD_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const BUILD_SCRIPT_FILES: &[&str] = &["build.rs"];

struct SpaceTermPatch {
    relative_path: &'static str,
    compiled_source: &'static [u8],
}

const SPACETERM_PATCHES: &[SpaceTermPatch] = &[
    SpaceTermPatch {
        relative_path: "patches/spaceterm-kitty-graphics.patch",
        compiled_source: include_bytes!("patches/spaceterm-kitty-graphics.patch"),
    },
    SpaceTermPatch {
        relative_path: "patches/spaceterm-terminal-effects.patch",
        compiled_source: include_bytes!("patches/spaceterm-terminal-effects.patch"),
    },
    SpaceTermPatch {
        relative_path: "patches/spaceterm-accessibility.patch",
        compiled_source: include_bytes!("patches/spaceterm-accessibility.patch"),
    },
];

#[derive(Clone, Copy)]
enum LinkMode {
    Dynamic,
    Static,
}

impl LinkMode {
    fn current() -> Self {
        if cfg!(feature = "link-dynamic") {
            Self::Dynamic
        } else {
            Self::Static
        }
    }

    fn artifact_kind(self) -> &'static str {
        match self {
            Self::Dynamic => "shared library",
            Self::Static => "static library",
        }
    }

    fn matches_library(self, target: &str, file_name: &str) -> bool {
        match self {
            Self::Dynamic => {
                if target.contains("darwin") {
                    file_name.starts_with("libghostty-vt") && file_name.ends_with(".dylib")
                } else if target.contains("windows") {
                    file_name == "ghostty-vt.lib"
                        || file_name == "ghostty-vt.dll"
                        || file_name == "libghostty-vt.dll.lib"
                        || file_name == "libghostty-vt.dll.a"
                } else {
                    file_name == "libghostty-vt.so" || file_name.starts_with("libghostty-vt.so.")
                }
            }
            Self::Static => {
                if target.contains("windows") {
                    file_name == "ghostty-vt-static.lib"
                } else {
                    file_name == "libghostty-vt.a"
                }
            }
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    let manifest_dir = manifest_dir();

    // docs.rs has no Zig toolchain. The checked-in bindings in src/bindings.rs
    // are enough for generating documentation, so skip the entire native
    // build when running under docs.rs.
    if env::var("DOCS_RS").is_ok() {
        return;
    }

    let link_mode = LinkMode::current();

    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SYS_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=GHOSTTY_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=GHOSTTY_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=DEBUG");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
    for build_script in BUILD_SCRIPT_FILES {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(build_script).display()
        );
    }
    for patch in SPACETERM_PATCHES {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(patch.relative_path).display()
        );
    }
    verify_compiled_patch_inputs(&manifest_dir);

    build_vendored(link_mode);
}

fn manifest_dir() -> PathBuf {
    let runtime_manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir must be set"));
    let compiled_manifest_dir = Path::new(BUILD_MANIFEST_DIR);
    assert_eq!(
        runtime_manifest_dir, compiled_manifest_dir,
        "Cargo reused a libghostty-vt-sys build script compiled for a different worktree; \
         remove this package's build-script artifacts from the shared target directory"
    );
    runtime_manifest_dir
}

fn verify_compiled_patch_inputs(manifest_dir: &Path) {
    for patch in SPACETERM_PATCHES {
        let path = manifest_dir.join(patch.relative_path);
        let on_disk = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert_eq!(
            on_disk,
            patch.compiled_source,
            "compiled build script contains stale patch data for {}; remove this package's \
             build-script artifacts from the shared target directory",
            path.display()
        );
    }
}

/// Build libghostty-vt from source via zig. The zig build itself generates
/// shared and static artifacts plus pkg-config files in `share/pkgconfig/`.
fn build_vendored(link_mode: LinkMode) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let target = env::var("TARGET").expect("TARGET must be set");
    let host = env::var("HOST").expect("HOST must be set");

    // A source override is an explicitly prepared checkout, primarily for binding updates.
    let ghostty_dir = match env::var("GHOSTTY_SOURCE_DIR") {
        Ok(dir) => {
            let p = PathBuf::from(dir);
            assert!(
                p.join("build.zig").exists(),
                "GHOSTTY_SOURCE_DIR does not contain build.zig: {}",
                p.display()
            );
            for input in ["build.zig", "build.zig.zon", "src", "include", "pkg"] {
                println!("cargo:rerun-if-changed={}", p.join(input).display());
            }
            p
        }
        Err(_) => prepare_ghostty(&out_dir),
    };
    verify_required_source_exports(&ghostty_dir);

    // Build libghostty-vt via zig.
    let install_prefix = out_dir.join("ghostty-install");
    let zig_cache_dir = out_dir.join("zig-cache");
    let zig_global_cache_dir = out_dir.join("zig-global-cache");

    let optimize = zig_optimize_mode();

    let mut build = Command::new("zig");
    build
        .arg("build")
        .arg("-Demit-lib-vt=true")
        .arg(format!("-Doptimize={optimize}"))
        .arg("-Dcpu=baseline")
        .arg("-Demit-xcframework=false")
        .arg("-Dapp-runtime=none")
        .arg("--prefix")
        .arg(&install_prefix)
        .arg("--cache-dir")
        .arg(&zig_cache_dir)
        .current_dir(&ghostty_dir);

    // Package managers can provide Ghostty's Zig package cache ahead of time
    // and ask Zig to resolve packages from that immutable store path instead
    // of fetching during this Cargo build script.
    if let Ok(dir) = env::var("GHOSTTY_ZIG_SYSTEM_DIR") {
        assert!(
            !dir.is_empty(),
            "GHOSTTY_ZIG_SYSTEM_DIR must not be empty when set"
        );
        let zig_system_dir = PathBuf::from(dir);
        assert!(
            zig_system_dir.exists(),
            "GHOSTTY_ZIG_SYSTEM_DIR does not exist: {}",
            zig_system_dir.display()
        );
        build
            .arg("--system")
            .arg(&zig_system_dir)
            .arg("--global-cache-dir")
            .arg(&zig_global_cache_dir);
    }

    // Only pass -Dtarget when cross-compiling. For native builds, let zig
    // auto-detect the host (matches how ghostty's own CMakeLists.txt works).
    if target != host {
        let zig_target = zig_target(&target);
        build.arg(format!("-Dtarget={zig_target}"));
    }

    run(build, "zig build");

    let lib_dir = install_prefix.join("lib");
    let include_dir = install_prefix.join("include");
    println!(
        "cargo:rustc-env=SPACETERM_GHOSTTY_INCLUDE_DIR={}",
        include_dir.display()
    );
    let search_dirs = library_search_dirs(&target, &install_prefix);
    warn_unused_xcframework(&lib_dir);

    let requested_libraries = search_dirs
        .iter()
        .flat_map(|dir| {
            std::fs::read_dir(dir)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
                .filter_map(|entry| {
                    let entry = entry.unwrap_or_else(|error| {
                        panic!("failed to read entry from {}: {error}", dir.display())
                    });
                    let file_name = entry.file_name();
                    let file_name = file_name.to_str()?;
                    link_mode
                        .matches_library(&target, file_name)
                        .then(|| entry.path())
                })
        })
        .collect::<Vec<_>>();
    assert!(
        !requested_libraries.is_empty(),
        "expected libghostty-vt {} in one of {:?}",
        link_mode.artifact_kind(),
        search_dirs
    );
    verify_required_library_exports(&target, link_mode, &requested_libraries);
    assert!(
        include_dir.join("ghostty").join("vt.h").exists(),
        "expected header at {}",
        include_dir.join("ghostty").join("vt.h").display()
    );

    for dir in &search_dirs {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    match link_mode {
        LinkMode::Dynamic => println!("cargo:rustc-link-lib=dylib=ghostty-vt"),
        LinkMode::Static => println!("cargo:rustc-link-lib=static=ghostty-vt"),
    }
    emit_include_metadata(&[include_dir]);
}

fn verify_required_source_exports(ghostty_dir: &Path) {
    for (header, symbols) in [
        (
            "grid_ref.h",
            &["ghostty_grid_ref_hyperlink_userdata"] as &[_],
        ),
        (
            "accessibility.h",
            &[
                "ghostty_accessibility_state_new",
                "ghostty_accessibility_state_free",
                "ghostty_accessibility_state_update",
                "ghostty_accessibility_state_set_selection",
            ],
        ),
    ] {
        let source = std::fs::read_to_string(ghostty_dir.join("include/ghostty/vt").join(header))
            .expect("patched Ghostty header must exist");
        for symbol in symbols {
            assert!(
                source.contains(symbol),
                "patched Ghostty header is missing {symbol}"
            );
        }
    }
}

fn verify_required_library_exports(
    target: &str,
    link_mode: LinkMode,
    requested_libraries: &[PathBuf],
) {
    if !target.contains("darwin") || !matches!(link_mode, LinkMode::Static) {
        return;
    }

    const REQUIRED_SYMBOLS: &[&str] = &[
        "ghostty_grid_ref_hyperlink_userdata",
        "ghostty_accessibility_state_new",
        "ghostty_accessibility_state_free",
        "ghostty_accessibility_state_update",
        "ghostty_accessibility_state_set_selection",
    ];
    for library in requested_libraries {
        let output = Command::new("nm")
            .arg("-gU")
            .arg(library)
            .output()
            .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", library.display()));
        assert!(
            output.status.success(),
            "nm -gU failed while inspecting {}: {}",
            library.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        let symbols = String::from_utf8_lossy(&output.stdout);
        for required_symbol in REQUIRED_SYMBOLS {
            let defines_required_symbol = symbols
                .lines()
                .filter_map(|line| line.split_whitespace().last())
                .any(|symbol| symbol.trim_start_matches('_') == *required_symbol);
            assert!(
                defines_required_symbol,
                "built libghostty-vt archive {} does not define external symbol `{required_symbol}`",
                library.display()
            );
        }
    }
}

fn warn_unused_xcframework(lib_dir: &Path) {
    let xcframework = lib_dir.join("ghostty-vt.xcframework");
    if xcframework.exists() {
        println!(
            "cargo:warning=unused libghostty-vt XCFramework emitted at {}; Cargo links the dylib or archive directly",
            xcframework.display()
        );
    }
}

fn emit_include_metadata(include_paths: &[PathBuf]) {
    if include_paths.is_empty() {
        return;
    }

    let joined = env::join_paths(include_paths)
        .unwrap_or_else(|error| panic!("failed to join include paths for cargo metadata: {error}"));
    println!("cargo:include={}", joined.to_string_lossy());
}

/// Decide which Zig `OptimizeMode` to pass to `zig build`.
///
/// The `LIBGHOSTTY_VT_SYS_OPTIMIZE` environment variable overrides this unconditionally; accepted
/// values are the four Zig `OptimizeMode` names (`Debug`, `ReleaseSafe`, `ReleaseFast`,
/// `ReleaseSmall`).
///
/// Defaults to `ReleaseFast` for optimized builds. If `DEBUG` is `true` (as cargo sets for the
/// `dev` profile), `Debug` mode is used. Otherwise, if `OPT_LEVEL` is `s` or `z`, `ReleaseSmall`
/// is used.
fn zig_optimize_mode() -> &'static str {
    if let Ok(override_mode) = env::var("LIBGHOSTTY_VT_SYS_OPTIMIZE") {
        return match override_mode.as_str() {
            "Debug" => "Debug",
            "ReleaseSafe" => "ReleaseSafe",
            "ReleaseFast" => "ReleaseFast",
            "ReleaseSmall" => "ReleaseSmall",
            other => panic!(
                "LIBGHOSTTY_VT_SYS_OPTIMIZE must be one of Debug, ReleaseSafe, ReleaseFast, ReleaseSmall (got '{other}')"
            ),
        };
    }

    if env::var("DEBUG").as_deref() == Ok("true") {
        return "Debug";
    }

    match env::var("OPT_LEVEL").as_deref() {
        Ok("s") | Ok("z") => "ReleaseSmall",
        _ => "ReleaseFast",
    }
}

/// Create a local build copy, invalidated by either the source revision or patch contents.
fn prepare_ghostty(out_dir: &Path) -> PathBuf {
    let manifest_dir = manifest_dir();
    let source = manifest_dir.join("../ghostty");
    assert!(
        source.join("build.zig").is_file(),
        "Ghostty source is missing; run git submodule update --init --recursive"
    );
    let commit = git_output(&source, &["rev-parse", "HEAD"]);
    let git_dir = PathBuf::from(git_output(&source, &["rev-parse", "--absolute-git-dir"]));
    // Detached submodule updates change HEAD; also track refs for deliberate development
    // checkouts. The source copy always uses the committed tree, never local edits.
    for path in [
        source.join(".git"),
        git_dir.join("HEAD"),
        git_dir.join("refs"),
        git_dir.join("packed-refs"),
    ] {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let src_dir = out_dir.join("ghostty-src");
    let stamp = src_dir.join(".spaceterm-source-inputs");
    let mut inputs = commit.as_bytes().to_vec();
    for patch in SPACETERM_PATCHES {
        inputs.extend_from_slice(patch.relative_path.as_bytes());
        inputs.extend_from_slice(patch.compiled_source);
    }
    if std::fs::read(&stamp).is_ok_and(|existing| existing == inputs) {
        return src_dir;
    }
    if src_dir.exists() {
        std::fs::remove_dir_all(&src_dir)
            .unwrap_or_else(|e| panic!("failed to remove {}: {e}", src_dir.display()));
    }
    eprintln!("Preparing pinned Ghostty {commit}");
    let mut clone = Command::new("git");
    clone
        .arg("clone")
        .arg("--shared")
        .arg("--no-checkout")
        .arg(&source)
        .arg(&src_dir);
    run(clone, "copy pinned Ghostty repository");
    let mut checkout = Command::new("git");
    checkout
        .arg("checkout")
        .arg("--detach")
        .arg(&commit)
        .current_dir(&src_dir);
    run(checkout, "checkout pinned Ghostty commit");
    apply_spaceterm_patch(&src_dir);
    std::fs::write(&stamp, inputs).unwrap_or_else(|e| panic!("failed to write source stamp: {e}"));
    src_dir
}

fn git_output(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is required to build the pinned Ghostty source");
    assert!(
        output.status.success(),
        "cannot resolve the Ghostty submodule revision"
    );
    String::from_utf8(output.stdout)
        .expect("Git revision metadata must be UTF-8")
        .trim()
        .to_owned()
}

fn apply_spaceterm_patch(src_dir: &Path) {
    let manifest_dir = manifest_dir();
    for patch in SPACETERM_PATCHES {
        apply_patch(src_dir, &manifest_dir.join(patch.relative_path));
    }
}

fn apply_patch(src_dir: &Path, patch: &Path) {
    let already_applied = Command::new("git")
        .args(["apply", "--reverse", "--check"])
        .arg(&patch)
        .current_dir(src_dir)
        .output()
        .unwrap_or_else(|error| panic!("failed to verify SpaceTerm Ghostty patch: {error}"));
    if already_applied.status.success() {
        return;
    }

    let applies_cleanly = Command::new("git")
        .args(["apply", "--check"])
        .arg(&patch)
        .current_dir(src_dir)
        .output()
        .unwrap_or_else(|error| panic!("failed to check SpaceTerm Ghostty patch: {error}"));
    if applies_cleanly.status.success() {
        let mut apply = Command::new("git");
        apply.arg("apply").arg(&patch).current_dir(src_dir);
        run(apply, "apply SpaceTerm Ghostty patch");
        return;
    }

    panic!(
        "SpaceTerm Ghostty patch {} is neither applicable nor already applied\n\
         reverse check: {}\nforward check: {}",
        patch.display(),
        String::from_utf8_lossy(&already_applied.stderr),
        String::from_utf8_lossy(&applies_cleanly.stderr)
    );
}

fn run(mut command: Command, context: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to execute {context}: {error}"));
    assert!(status.success(), "{context} failed with status {status}");
}

/// Returns directories to search for the built library artifact.
/// On Windows, Zig may place the DLL in `bin/` and the import lib in `lib/`,
/// so both are included.
fn library_search_dirs(target: &str, install_prefix: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![install_prefix.join("lib")];
    if target.contains("windows") {
        dirs.push(install_prefix.join("bin"));
    }
    dirs
}

fn zig_target(target: &str) -> String {
    let value = match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "aarch64-apple-darwin" => "aarch64-macos-none",
        "x86_64-apple-darwin" => "x86_64-macos-none",
        "x86_64-pc-windows-gnu" => "x86_64-windows-gnu",
        "aarch64-pc-windows-gnullvm" => "aarch64-windows-gnu",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        "aarch64-linux-android" => "aarch64-linux-android",
        "x86_64-linux-android" => "x86_64-linux-android",
        other => panic!("unsupported Rust target for vendored build: {other}"),
    };
    value.to_owned()
}
