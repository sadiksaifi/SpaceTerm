//! The installed app's discovery and removal on a real file system, including the layouts whose
//! removal would delete the account's own directories.
use std::os::unix::fs::symlink;
use std::path::Path;

use super::*;

const ID: &str = "io.github.sadiksaifi.spaceterm";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "spaceterm-installed-app-{}-{:x}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }

    /// An account whose home is `<scratch>/home`, with SpaceTerm Core's data root in place.
    fn account(&self) -> Account {
        self.account_at(self.0.join("home"))
    }

    fn account_at(&self, home: PathBuf) -> Account {
        let core_data = home.join(".local/share/spaceterm/core");
        std::fs::create_dir_all(&core_data).unwrap();
        touch(&core_data.join("data"));
        std::fs::create_dir_all(self.0.join("Applications")).unwrap();
        Account {
            data_home: home.join(".local/share"),
            home,
            core_data,
            applications: self.0.join("Applications"),
            install_dir: None,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rand_suffix() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::SystemTime::now())
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"").unwrap();
}

fn release() -> AppIdentity {
    AppIdentity::for_identity("spaceterm")
}

/// `install.sh`'s `exec_value`: Exec quoting, then string escaping.
fn exec_value(program: &Path) -> String {
    let mut quoted = String::new();
    for character in program.to_str().unwrap().chars() {
        if matches!(character, '\\' | '"' | '`' | '$') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    format!("\"{}\"", quoted.replace('\\', "\\\\"))
}

/// A tree as `install.sh` lays it out under `install_dir`, with its launcher and desktop entry.
/// Returns the tree.
fn install_tree(install_dir: &Path, account: &Account) -> PathBuf {
    let root = install_dir.join("spaceterm");
    let executable = root.join("bin/spaceterm-desktop");
    touch(&executable);
    touch(&root.join("share/spaceterm/resources"));
    touch(&root.join(format!("share/applications/{ID}.desktop")));
    touch(&root.join(format!("share/icons/hicolor/scalable/apps/{ID}.svg")));
    let launcher = account.home.join(".local/bin/spaceterm-desktop");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    symlink(&executable, &launcher).unwrap();
    let entry = desktop_entry(&account.data_home, &release());
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(
        &entry,
        format!(
            "[Desktop Entry]\nName=SpaceTerm\nExec={} %u\n",
            exec_value(&executable)
        ),
    )
    .unwrap();
    root
}

fn tree(root: &Path, account: &Account) -> InstalledApp {
    InstalledApp::new(Location::Tree(root.to_path_buf()), &release(), account)
}

fn write_plist(bundle: &Path, id: &str) {
    touch(&bundle.join("Contents/MacOS/SpaceTerm"));
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n\t<key>CFBundleIdentifier</key>\n\t<string>{id}</string>\n</dict>\n</plist>\n"
        ),
    )
    .unwrap();
}

#[cfg(feature = "desktop")]
#[test]
fn identity_names_follow_the_build_identity() {
    let preflight = AppIdentity::for_identity("spaceterm-preflight");
    assert_eq!(
        preflight.application_id(),
        "io.github.sadiksaifi.spaceterm-preflight"
    );
    assert_eq!(preflight.bundle, "SpaceTerm Preflight.app");
    assert_eq!(release().bundle, "SpaceTerm.app");
    for identity in crate::application_identity::testing::all() {
        assert_eq!(
            AppIdentity::for_identity(identity.directory_name()).application_id(),
            identity.application_id()
        );
    }
}

#[test]
fn an_app_bundle_is_found_from_its_executable_and_removed_whole() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let bundle = scratch.0.join("Applications/SpaceTerm.app");
    write_plist(&bundle, ID);
    touch(&bundle.join("Contents/Resources/icon.icns"));

    let location = bundle_of(
        &bundle.join("Contents/MacOS/SpaceTerm"),
        &release(),
        &account,
    );
    assert_eq!(location, Some(Location::Bundle(bundle.clone())));
    assert_eq!(
        installed_bundles(&release(), &account),
        vec![Location::Bundle(bundle.clone())]
    );
    InstalledApp::new(location.unwrap(), &release(), &account)
        .remove()
        .unwrap();

    assert!(!bundle.exists());
    assert!(scratch.0.join("Applications").is_dir());
    assert_eq!(
        bundle_of(
            &scratch.0.join("target/debug/spaceterm"),
            &release(),
            &account
        ),
        None
    );
}

#[test]
fn a_bundle_of_another_identity_or_without_a_bundle_identifier_is_not_found() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let preflight = scratch.0.join("Applications/SpaceTerm.app");
    write_plist(&preflight, "io.github.sadiksaifi.spaceterm-preflight");
    let bare = account.home.join("Applications/SpaceTerm.app");
    touch(&bare.join("Contents/MacOS/SpaceTerm"));

    for bundle in [&preflight, &bare] {
        let executable = bundle.join("Contents/MacOS/SpaceTerm");
        assert_eq!(bundle_of(&executable, &release(), &account), None);
    }
    assert!(installed_bundles(&release(), &account).is_empty());
}

/// A home directory that ends in `.app` holds `Contents/MacOS` and an `Info.plist` that names
/// SpaceTerm, and still stays.
#[test]
fn a_bundle_that_is_the_home_directory_stays() {
    let scratch = Scratch::new();
    let home = scratch.0.join("SpaceTerm.app");
    let account = scratch.account_at(home.clone());
    write_plist(&home, ID);

    let executable = home.join("Contents/MacOS/SpaceTerm");
    assert_eq!(bundle_of(&executable, &release(), &account), None);
    assert!(
        InstalledApp::new(Location::Bundle(home.clone()), &release(), &account)
            .remove()
            .is_err()
    );
    assert!(executable.exists());
    assert!(account.core_data.join("data").exists());
}

#[test]
fn a_symbolic_link_in_place_of_the_bundle_is_refused() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let target = scratch.0.join("elsewhere/SpaceTerm.app");
    write_plist(&target, ID);
    let bundle = scratch.0.join("Applications/SpaceTerm.app");
    symlink(&target, &bundle).unwrap();

    assert!(installed_bundles(&release(), &account).is_empty());
    assert!(
        InstalledApp::new(Location::Bundle(bundle), &release(), &account)
            .remove()
            .is_err()
    );
    assert!(target.join("Contents/Info.plist").exists());
}

#[test]
fn an_installed_tree_is_found_and_removed_with_its_launcher_and_desktop_entry() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let root = install_tree(&account.home.join(".local/lib"), &account);
    touch(&account.home.join(".local/lib/.spaceterm.update/partial"));
    let other_entry = account.data_home.join("applications/other.desktop");
    touch(&other_entry);

    let found = tree_of(&root.join("bin/spaceterm-desktop"), &release(), &account);
    assert_eq!(found, Some(Location::Tree(root.clone())));
    assert_eq!(
        installed_trees(&release(), &account),
        vec![Location::Tree(root.clone())]
    );
    tree(&root, &account).remove().unwrap();

    assert!(!root.exists());
    assert!(!account.home.join(".local/lib/.spaceterm.update").exists());
    let launcher = account.home.join(".local/bin/spaceterm-desktop");
    assert!(std::fs::symlink_metadata(launcher).is_err());
    assert!(!desktop_entry(&account.data_home, &release()).exists());
    assert!(other_entry.exists(), "other desktop entries stay");
    assert!(account.core_data.join("data").exists());
}

/// `SPACETERM_INSTALL_DIR` put the tree in a path with spaces and characters the desktop entry
/// escapes. With the launcher gone, the desktop entry still finds it.
#[test]
fn a_tree_in_another_directory_is_found_through_its_desktop_entry() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let install_dir = scratch.0.join("Apps and $tools \"x\" `y` \\z");
    let root = install_tree(&install_dir, &account);
    std::fs::remove_file(account.home.join(".local/bin/spaceterm-desktop")).unwrap();

    assert_eq!(
        installed_trees(&release(), &account),
        vec![Location::Tree(root.clone())]
    );
    tree(&root, &account).remove().unwrap();
    assert!(!root.exists());
    assert!(!desktop_entry(&account.data_home, &release()).exists());
    assert!(install_dir.is_dir());
}

#[test]
fn exec_values_are_read_back_as_install_sh_wrote_them() {
    for program in [
        "/home/a/.local/lib/spaceterm/bin/spaceterm-desktop",
        "/opt/My Apps/$HOME \"q\" `c` \\b/spaceterm/bin/spaceterm-desktop",
    ] {
        let value = format!("{} %u", exec_value(Path::new(program)));
        assert_eq!(exec_program(&value).as_deref(), Some(program));
    }
    assert_eq!(
        exec_program("spaceterm-desktop %u").as_deref(),
        Some("spaceterm-desktop")
    );
    assert_eq!(exec_program("\"unterminated"), None);
}

/// `tar --strip-components=1 -C ~/.local` puts the release's files straight into `~/.local`,
/// beside SpaceTerm Core's data in `~/.local/share/spaceterm`. The launcher is then a real file.
#[test]
fn a_release_unpacked_into_dot_local_is_not_offered_and_nothing_is_removed() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let local = account.home.join(".local");
    let executable = local.join("bin/spaceterm-desktop");
    touch(&executable);
    touch(&local.join("share/spaceterm/resources"));
    touch(&local.join(format!("share/applications/{ID}.desktop")));
    std::fs::write(
        local.join(format!("share/applications/{ID}.desktop")),
        format!("[Desktop Entry]\nExec={} %u\n", exec_value(&executable)),
    )
    .unwrap();
    touch(&local.join("state/keyring/login.keyring"));

    assert_eq!(tree_of(&executable, &release(), &account), None);
    assert_eq!(
        InstalledApp::running(&executable, &release(), &account),
        None
    );
    assert!(installed_trees(&release(), &account).is_empty());
    assert!(tree(&local, &account).remove().is_err());

    assert!(executable.exists());
    assert!(local.join("state/keyring/login.keyring").exists());
    assert!(account.core_data.join("data").exists());
}

/// A distribution's layout puts `bin/spaceterm-desktop` beside `share/spaceterm` in `/usr`.
#[test]
fn a_system_prefix_is_not_offered() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let usr = scratch.0.join("usr");
    touch(&usr.join("bin/spaceterm-desktop"));
    touch(&usr.join("share/spaceterm/resources"));
    touch(&usr.join(format!("share/applications/{ID}.desktop")));

    assert_eq!(
        tree_of(&usr.join("bin/spaceterm-desktop"), &release(), &account),
        None
    );
    assert!(tree(&usr, &account).remove().is_err());
    assert!(usr.join("bin/spaceterm-desktop").exists());
}

/// A directory named `spaceterm` that holds the tree's files but is, or contains, the home
/// directory, the data home or SpaceTerm Core's data root stays.
#[test]
fn a_tree_that_holds_the_accounts_own_directories_stays() {
    let scratch = Scratch::new();
    let home = scratch.0.join("spaceterm");
    let mut account = scratch.account_at(home.clone());
    let share_spaceterm = home.join(".local/share/spaceterm");
    let data_home = scratch.0.join("data/spaceterm");
    std::fs::create_dir_all(&data_home).unwrap();
    for root in [&home, &share_spaceterm, &data_home] {
        touch(&root.join("bin/spaceterm-desktop"));
        touch(&root.join(format!("share/applications/{ID}.desktop")));
    }
    account.data_home = data_home.clone();

    for root in [&home, &share_spaceterm, &data_home] {
        let executable = root.join("bin/spaceterm-desktop");
        assert_eq!(tree_of(&executable, &release(), &account), None, "{root:?}");
        assert!(tree(root, &account).remove().is_err(), "{root:?}");
        assert!(executable.exists(), "{root:?}");
    }
    assert!(account.core_data.join("data").exists());
}

/// `~/.local/lib/spaceterm` is a symbolic link to `~/.local`. The installed tree is where the link
/// resolves, and that is not a tree.
#[test]
fn a_tree_reached_through_a_symbolic_link_to_dot_local_is_not_offered() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let local = account.home.join(".local");
    touch(&local.join("bin/spaceterm-desktop"));
    touch(&local.join(format!("share/applications/{ID}.desktop")));
    std::fs::create_dir_all(local.join("lib")).unwrap();
    symlink(&local, local.join("lib/spaceterm")).unwrap();

    let executable = local.join("lib/spaceterm/bin/spaceterm-desktop");
    assert_eq!(
        InstalledApp::running(&executable, &release(), &account),
        None
    );
    assert!(installed_trees(&release(), &account).is_empty());
    assert!(
        tree(&local.join("lib/spaceterm"), &account)
            .remove()
            .is_err()
    );
    assert!(local.join("bin/spaceterm-desktop").exists());
    assert!(account.core_data.join("data").exists());
}

/// Removal deletes the links inside the tree, never what they point at.
#[test]
fn symbolic_links_inside_the_tree_are_removed_without_their_targets() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let root = install_tree(&account.home.join(".local/lib"), &account);
    touch(&account.home.join("Documents/kept"));
    symlink(account.home.join("Documents"), root.join("share/documents")).unwrap();
    symlink(&account.home, root.join("home")).unwrap();

    tree(&root, &account).remove().unwrap();

    assert!(!root.exists());
    assert!(account.home.join("Documents/kept").exists());
    assert!(account.core_data.join("data").exists());
}

/// Preflight installs beside Release under its own names.
#[test]
fn a_preflight_tree_is_removed_with_its_own_launcher_and_leaves_release_alone() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let preflight = AppIdentity::for_identity("spaceterm-preflight");
    let lib = account.home.join(".local/lib");
    let bin = account.home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, id) in [
        ("spaceterm", ID),
        ("spaceterm-preflight", preflight.application_id()),
    ] {
        touch(&lib.join(name).join("bin/spaceterm-desktop"));
        touch(
            &lib.join(name)
                .join(format!("share/applications/{id}.desktop")),
        );
        symlink(
            lib.join(name).join("bin/spaceterm-desktop"),
            bin.join(format!("{name}-desktop")),
        )
        .unwrap();
    }

    let found = installed_trees(&preflight, &account);
    assert_eq!(found, vec![Location::Tree(lib.join("spaceterm-preflight"))]);
    InstalledApp::new(found[0].clone(), &preflight, &account)
        .remove()
        .unwrap();

    assert!(!lib.join("spaceterm-preflight").exists());
    assert!(std::fs::symlink_metadata(bin.join("spaceterm-preflight-desktop")).is_err());
    assert!(lib.join("spaceterm/bin/spaceterm-desktop").exists());
    assert_eq!(
        std::fs::read_link(bin.join("spaceterm-desktop")).unwrap(),
        lib.join("spaceterm/bin/spaceterm-desktop"),
        "Release's launcher stays"
    );
}

#[test]
fn a_launcher_and_desktop_entry_of_another_installation_stay() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let root = install_tree(&account.home.join(".local/lib"), &account);
    let foreign = install_tree_elsewhere(&scratch, &account);

    tree(&root, &account).remove().unwrap();

    assert!(!root.exists());
    let launcher = account.home.join(".local/bin/spaceterm-desktop");
    assert_eq!(std::fs::read_link(&launcher).unwrap(), foreign);
    assert!(desktop_entry(&account.data_home, &release()).exists());
}

/// Points the launcher and desktop entry at another program, as a second installation would.
/// Returns that program.
fn install_tree_elsewhere(scratch: &Scratch, account: &Account) -> PathBuf {
    let foreign = scratch.0.join("elsewhere/spaceterm-desktop");
    touch(&foreign);
    let launcher = account.home.join(".local/bin/spaceterm-desktop");
    std::fs::remove_file(&launcher).unwrap();
    symlink(&foreign, &launcher).unwrap();
    std::fs::write(
        desktop_entry(&account.data_home, &release()),
        format!("[Desktop Entry]\nExec={} %u\n", exec_value(&foreign)),
    )
    .unwrap();
    foreign
}

/// A copy of `sleep` that runs until dropped.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs a copy of `sleep` as `executable` and returns once the process list shows it.
/// `spawn` can return before the child has executed the copy.
fn run_as(executable: &Path) -> Running {
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(executable);
    std::fs::copy("/bin/sleep", executable).unwrap();
    let running = Running(spawn(executable));
    let executable = std::fs::canonicalize(executable).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !super::running_executables().contains(&executable) {
        assert!(
            std::time::Instant::now() < deadline,
            "the copy of sleep never ran"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    running
}

fn spawn(executable: &Path) -> std::process::Child {
    // Another test's fork can briefly hold the fresh copy open for writing.
    for _ in 0..50 {
        match std::process::Command::new(executable).arg("30").spawn() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result.unwrap(),
        }
    }
    panic!("the copy of sleep stayed busy");
}

#[cfg(target_os = "linux")]
#[test]
fn a_tree_runs_while_its_executable_or_a_staged_copy_does() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let root = install_tree(&scratch.0.join("lib"), &account);
    let app = tree(&root, &account);
    assert!(!app.is_running());

    let child = run_as(&root.join("bin/spaceterm-desktop"));
    assert!(app.is_running());
    drop(child);
    assert!(!app.is_running());

    let staged = run_as(
        &scratch
            .0
            .join("lib/.spaceterm.update/next/bin/spaceterm-desktop"),
    );
    assert!(app.is_running());
    drop(staged);

    let elsewhere = run_as(&scratch.0.join("other/spaceterm/bin/spaceterm-desktop"));
    assert!(!app.is_running());
    drop(elsewhere);
}

#[cfg(target_os = "macos")]
#[test]
fn a_bundle_runs_while_its_executable_does() {
    let scratch = Scratch::new();
    let account = scratch.account();
    let bundle = scratch.0.join("Applications/SpaceTerm.app");
    write_plist(&bundle, ID);
    let app = InstalledApp::new(Location::Bundle(bundle.clone()), &release(), &account);
    assert!(!app.is_running());

    let child = run_as(&bundle.join("Contents/MacOS/SpaceTerm"));
    assert!(app.is_running());
    drop(child);
    assert!(!app.is_running());

    let elsewhere = run_as(
        &scratch
            .0
            .join("Other/SpaceTerm.app/Contents/MacOS/SpaceTerm"),
    );
    assert!(!app.is_running());
    drop(elsewhere);
}
