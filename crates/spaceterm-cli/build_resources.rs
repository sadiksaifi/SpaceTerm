use std::{fs, path::Path, process::Command};

pub fn stage(root: &Path, output: &Path) {
    let source = root.join("assets");
    let terminfo = source.join("terminfo/xterm-spaceterm.terminfo");
    let shell_integration = source.join("shell-integration");
    println!("cargo:rerun-if-changed={}", terminfo.display());
    println!("cargo:rerun-if-changed={}", shell_integration.display());
    let resources = output.join("resources");
    if resources.exists() {
        fs::remove_dir_all(&resources).expect("remove previous Development resources");
    }
    copy_tree(&shell_integration, &resources.join("shell-integration"));
    // Match the packaging tasks: compile the tracked source with extended capabilities.
    let status = Command::new("tic")
        .args(["-x", "-o"])
        .arg(resources.join("terminfo"))
        .arg(terminfo)
        .status()
        .expect("Development resources require tic; install ncurses (ncurses-bin on Debian or Ubuntu) and ensure tic is on PATH");
    assert!(
        status.success(),
        "tic -x could not compile the SpaceTerm Development terminfo entry"
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create Development shell integration directory");
    for entry in fs::read_dir(source).expect("read tracked shell integration directory") {
        let entry = entry.expect("read tracked shell integration entry");
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("read shell integration type")
            .is_dir()
        {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy tracked shell integration resource");
        }
    }
}
