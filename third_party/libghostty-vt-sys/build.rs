use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned ghostty commit. Update this to pull a newer version.
const GHOSTTY_REPO: &str = "https://github.com/ghostty-org/ghostty.git";
const GHOSTTY_COMMIT: &str = "a887df42c56f6de86c0fe6da9c4eeca37931e083";
const BUILD_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const BUILD_SCRIPT_FILES: &[&str] = &["spaceterm-accessibility-build.rs", "build.rs"];

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

    #[cfg(feature = "pkg-config")]
    fn pkg_config_name(self) -> &'static str {
        match self {
            Self::Dynamic => "libghostty-vt",
            Self::Static => "libghostty-vt-static",
        }
    }
}

fn main() {
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

    // An explicit source override should stay authoritative even when the
    // pkg-config feature is enabled, so local Ghostty checkouts remain easy to
    // test against.
    if env::var_os("GHOSTTY_SOURCE_DIR").is_some() {
        build_vendored(link_mode);
        return;
    }

    // When the pkg-config feature is enabled, prefer an installed library over
    // fetching Ghostty. libghostty is pre-1.0, so this crate intentionally does
    // not promise compatibility with every installed C API revision.
    #[cfg(feature = "pkg-config")]
    if try_pkg_config(link_mode) {
        return;
    }

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

    // Locate ghostty source: env override > fetch into OUT_DIR.
    let ghostty_dir = match env::var("GHOSTTY_SOURCE_DIR") {
        Ok(dir) => {
            let p = PathBuf::from(dir);
            assert!(
                p.join("build.zig").exists(),
                "GHOSTTY_SOURCE_DIR does not contain build.zig: {}",
                p.display()
            );
            p
        }
        Err(_) => fetch_ghostty(&out_dir),
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
    const REQUIRED_EXPORTS: &[(&str, &str)] = &[
        (
            "include/ghostty/vt.h",
            "#include <ghostty/vt/accessibility.h>",
        ),
        (
            "include/ghostty/vt/accessibility.h",
            "ghostty_accessibility_state_new(",
        ),
        (
            "include/ghostty/vt/accessibility.h",
            "ghostty_accessibility_state_free(",
        ),
        (
            "include/ghostty/vt/accessibility.h",
            "ghostty_accessibility_state_update(",
        ),
        (
            "include/ghostty/vt/accessibility.h",
            "ghostty_accessibility_state_set_selection(",
        ),
        ("src/terminal/c/accessibility.zig", "pub fn state_new("),
        ("src/terminal/c/accessibility.zig", "pub fn state_free("),
        ("src/terminal/c/accessibility.zig", "pub fn state_update("),
        (
            "src/terminal/c/accessibility.zig",
            "pub fn state_set_selection(",
        ),
        (
            "src/terminal/c/main.zig",
            "pub const accessibility_state_new = accessibility.state_new;",
        ),
        (
            "src/terminal/c/main.zig",
            "pub const accessibility_state_free = accessibility.state_free;",
        ),
        (
            "src/terminal/c/main.zig",
            "pub const accessibility_state_update = accessibility.state_update;",
        ),
        (
            "src/terminal/c/main.zig",
            "pub const accessibility_state_set_selection = accessibility.state_set_selection;",
        ),
        (
            "src/lib_vt.zig",
            "@export(&c.accessibility_state_new, .{ .name = \"ghostty_accessibility_state_new\" });",
        ),
        (
            "src/lib_vt.zig",
            "@export(&c.accessibility_state_free, .{ .name = \"ghostty_accessibility_state_free\" });",
        ),
        (
            "src/lib_vt.zig",
            "@export(&c.accessibility_state_update, .{ .name = \"ghostty_accessibility_state_update\" });",
        ),
        (
            "src/lib_vt.zig",
            "@export(&c.accessibility_state_set_selection, .{ .name = \"ghostty_accessibility_state_set_selection\" });",
        ),
    ];

    for (relative_path, required_source) in REQUIRED_EXPORTS {
        let path = ghostty_dir.join(relative_path);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert!(
            source.contains(required_source),
            "patched Ghostty source {} is missing required export `{required_source}`",
            path.display()
        );
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

#[cfg(feature = "pkg-config")]
fn try_pkg_config(link_mode: LinkMode) -> bool {
    let mut config = pkg_config::Config::new();
    let lib = match link_mode {
        LinkMode::Dynamic => config.probe(link_mode.pkg_config_name()),
        LinkMode::Static => config
            .statik(true)
            .cargo_metadata(false)
            .probe(link_mode.pkg_config_name()),
    };
    let lib = match lib {
        Ok(lib) => lib,
        Err(_) => return false,
    };

    if let LinkMode::Static = link_mode {
        emit_static_pkg_config_metadata(&lib);
    }
    emit_include_metadata(&lib.include_paths);
    true
}

#[cfg(feature = "pkg-config")]
fn emit_static_pkg_config_metadata(lib: &pkg_config::Library) {
    for path in &lib.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for path in &lib.link_files {
        if let Some(parent) = path.parent() {
            println!("cargo:rustc-link-search=native={}", parent.display());
        }
    }
    for path in &lib.framework_paths {
        println!("cargo:rustc-link-search=framework={}", path.display());
    }
    for framework in &lib.frameworks {
        println!("cargo:rustc-link-lib=framework={framework}");
    }

    println!("cargo:rustc-link-lib=static=ghostty-vt");
    for library in &lib.libs {
        if library != "ghostty-vt" {
            println!("cargo:rustc-link-lib={library}");
        }
    }
    for args in &lib.ld_args {
        if !args.is_empty() {
            println!("cargo:rustc-link-arg=-Wl,{}", args.join(","));
        }
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

/// Clone ghostty at the pinned commit into OUT_DIR/ghostty-src.
/// Reuses an existing clone if the commit matches.
fn fetch_ghostty(out_dir: &Path) -> PathBuf {
    let src_dir = out_dir.join("ghostty-src");
    let stamp = src_dir.join(".ghostty-commit");

    // Skip fetch if we already have the right commit.
    if stamp.exists()
        && let Ok(existing) = std::fs::read_to_string(&stamp)
        && existing.trim() == GHOSTTY_COMMIT
    {
        apply_spaceterm_patch(&src_dir);
        return src_dir;
    }

    // Clean and clone fresh.
    if src_dir.exists() {
        std::fs::remove_dir_all(&src_dir)
            .unwrap_or_else(|e| panic!("failed to remove {}: {e}", src_dir.display()));
    }

    eprintln!("Fetching ghostty {GHOSTTY_COMMIT} ...");

    let mut clone = Command::new("git");
    clone
        .arg("clone")
        .arg("--filter=blob:none")
        .arg("--no-checkout")
        .arg(GHOSTTY_REPO)
        .arg(&src_dir);
    run(clone, "git clone ghostty");

    let mut checkout = Command::new("git");
    checkout
        .arg("checkout")
        .arg(GHOSTTY_COMMIT)
        .current_dir(&src_dir);
    run(checkout, "git checkout ghostty commit");

    apply_spaceterm_patch(&src_dir);

    std::fs::write(&stamp, GHOSTTY_COMMIT).unwrap_or_else(|e| panic!("failed to write stamp: {e}"));

    src_dir
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
