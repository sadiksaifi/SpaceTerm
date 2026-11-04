//! The SpaceTerm desktop app as its installer laid it out, and its removal. Settings > Uninstall
//! SpaceTerm removes the app it runs from. `spaceterm uninstall` removes the app it finds in the
//! places the installer uses.
//!
//! Removal deletes a whole directory, so only a layout that names SpaceTerm qualifies. On macOS
//! that is the `.app` bundle whose `Info.plist` carries this identity's bundle identifier. On
//! Linux it is the tree `install.sh` creates: a directory named after the identity, such as
//! `spaceterm`, holding `bin/spaceterm-desktop` and `share/applications/<id>.desktop`. A
//! directory that is or contains the home directory, the XDG data home or SpaceTerm Core's data
//! root never qualifies, whatever it holds.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

const EXECUTABLE: &str = "spaceterm-desktop";

/// The names one build identity installs under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppIdentity {
    /// The Linux tree's directory name, such as `spaceterm`.
    directory_name: String,
    /// The bundle identifier and desktop entry ID, such as `io.github.sadiksaifi.spaceterm`.
    application_id: String,
    /// The macOS bundle's file name, such as `SpaceTerm.app`.
    bundle: String,
}

impl AppIdentity {
    /// The names of the build identity `identity`, such as `spaceterm-preflight`.
    pub fn for_identity(identity: &str) -> Self {
        Self {
            directory_name: identity.to_owned(),
            application_id: format!("io.github.sadiksaifi.{identity}"),
            bundle: format!(
                "{}.app",
                spaceterm_core::service::control::display_name(identity)
            ),
        }
    }

    pub fn application_id(&self) -> &str {
        &self.application_id
    }
}

/// This account's directories that a removal never deletes, and where the installer puts the app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Account {
    home: PathBuf,
    data_home: PathBuf,
    core_data: PathBuf,
    /// The shared Applications folder on macOS.
    applications: PathBuf,
    /// `SPACETERM_INSTALL_DIR`, which selects another directory for the app.
    install_dir: Option<PathBuf>,
}

impl Account {
    /// This account's directories for the build identity `identity`, or `None` without an
    /// absolute HOME or a SpaceTerm Core data root, so nothing is removed.
    pub fn current(identity: &str) -> Option<Self> {
        let environment = crate::platform::app_directories::AppDirectoryEnvironment::capture();
        let home = absolute(environment.home)?;
        let data_home = absolute(environment.xdg_data_home)
            .unwrap_or_else(|| home.join(".local").join("share"));
        let core_data = spaceterm_core::core_data_root(identity).ok()?;
        Some(Self {
            home: real(&home),
            data_home: real(&data_home),
            core_data: real(&core_data),
            applications: PathBuf::from("/Applications"),
            install_dir: absolute(std::env::var_os("SPACETERM_INSTALL_DIR")),
        })
    }

    /// Whether removing `directory` would delete one of the account's own directories.
    fn protects(&self, directory: &Path) -> bool {
        [&self.home, &self.data_home, &self.core_data]
            .into_iter()
            .any(|kept| kept.starts_with(directory))
    }
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// `path` with its symbolic links resolved, or as given when it doesn't exist.
fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One installed SpaceTerm desktop app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledApp {
    location: Location,
    identity: AppIdentity,
    account: Account,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Location {
    /// `<bundle>.app`, removed whole.
    Bundle(PathBuf),
    /// `<root>/bin/spaceterm-desktop`, with the launcher, icon and desktop entry `install.sh`
    /// added for it.
    Tree(PathBuf),
}

impl InstalledApp {
    /// The installed app running `executable`, or `None` for a build that runs from elsewhere,
    /// such as a source checkout.
    pub fn running(executable: &Path, identity: &AppIdentity, account: &Account) -> Option<Self> {
        let executable = std::fs::canonicalize(executable).ok()?;
        let location = if cfg!(target_os = "macos") {
            bundle_of(&executable, identity, account)
        } else {
            tree_of(&executable, identity, account)
        }?;
        Some(Self::new(location, identity, account))
    }

    /// The installed app of the build identity `identity` that runs this process, or `None`
    /// for a build that runs from elsewhere.
    pub fn current(identity: &str) -> Option<Self> {
        Self::running(
            &std::env::current_exe().ok()?,
            &AppIdentity::for_identity(identity),
            &Account::current(identity)?,
        )
    }

    /// The apps of `identity` installed where the installer puts them: on macOS
    /// `/Applications` and `~/Applications`, on Linux the tree the launcher or desktop entry
    /// names and `~/.local/lib/<identity>`, and `SPACETERM_INSTALL_DIR` when set.
    pub fn installed(identity: &AppIdentity, account: &Account) -> Vec<Self> {
        let locations = if cfg!(target_os = "macos") {
            installed_bundles(identity, account)
        } else {
            installed_trees(identity, account)
        };
        locations
            .into_iter()
            .map(|location| Self::new(location, identity, account))
            .collect()
    }

    fn new(location: Location, identity: &AppIdentity, account: &Account) -> Self {
        Self {
            location,
            identity: identity.clone(),
            account: account.clone(),
        }
    }

    /// Removes what the installer installed for this app and nothing else. The layout is checked
    /// again first, so a directory that changed since it was found stays.
    pub fn remove(&self) -> io::Result<()> {
        match &self.location {
            Location::Bundle(bundle) => {
                if !is_bundle(bundle, &self.identity, &self.account) {
                    return Err(io::ErrorKind::InvalidInput.into());
                }
                remove_directory(bundle)
            }
            Location::Tree(root) => {
                if !is_tree(root, &self.identity, &self.account) {
                    return Err(io::ErrorKind::InvalidInput.into());
                }
                remove_tree(root, &self.identity, &self.account)
            }
        }
    }
}

/// The bundle `<bundle>.app/Contents/MacOS/<executable>` names.
fn bundle_of(executable: &Path, identity: &AppIdentity, account: &Account) -> Option<Location> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && is_bundle(bundle, identity, account))
    .then(|| Location::Bundle(bundle.to_path_buf()))
}

fn is_bundle(bundle: &Path, identity: &AppIdentity, account: &Account) -> bool {
    bundle
        .extension()
        .is_some_and(|extension| extension == "app")
        && is_real_directory(bundle)
        && !account.protects(bundle)
        && std::fs::read_to_string(bundle.join("Contents/Info.plist"))
            .is_ok_and(|plist| bundle_identifier(&plist) == Some(identity.application_id.as_str()))
}

/// The `CFBundleIdentifier` of an XML property list.
fn bundle_identifier(plist: &str) -> Option<&str> {
    let (_, rest) = plist.split_once("<key>CFBundleIdentifier</key>")?;
    let rest = rest.trim_start().strip_prefix("<string>")?;
    let (value, _) = rest.split_once("</string>")?;
    Some(value.trim())
}

/// The tree `<root>/bin/spaceterm-desktop` names.
fn tree_of(executable: &Path, identity: &AppIdentity, account: &Account) -> Option<Location> {
    let bin = executable.parent()?;
    let root = bin.parent()?;
    (executable.file_name()? == EXECUTABLE
        && bin.file_name()? == "bin"
        && is_tree(root, identity, account))
    .then(|| Location::Tree(root.to_path_buf()))
}

fn is_tree(root: &Path, identity: &AppIdentity, account: &Account) -> bool {
    root.file_name()
        .is_some_and(|name| name == identity.directory_name.as_str())
        && is_real_directory(root)
        && !account.protects(root)
        && std::fs::symlink_metadata(root.join("bin").join(EXECUTABLE))
            .is_ok_and(|metadata| metadata.is_file())
        && std::fs::symlink_metadata(
            root.join("share/applications")
                .join(format!("{}.desktop", identity.application_id)),
        )
        .is_ok_and(|metadata| metadata.is_file())
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn installed_bundles(identity: &AppIdentity, account: &Account) -> Vec<Location> {
    let mut directories = vec![
        account.applications.clone(),
        account.home.join("Applications"),
    ];
    directories.extend(account.install_dir.clone());
    let mut found = Vec::new();
    for directory in directories {
        let bundle = real(&directory).join(&identity.bundle);
        if is_bundle(&bundle, identity, account) && !found.contains(&bundle) {
            found.push(bundle);
        }
    }
    found.into_iter().map(Location::Bundle).collect()
}

fn installed_trees(identity: &AppIdentity, account: &Account) -> Vec<Location> {
    let mut executables = Vec::new();
    let launcher = launcher(&account.home, &identity.directory_name);
    if let Ok(target) = std::fs::read_link(&launcher) {
        executables.push(
            launcher
                .parent()
                .map_or(target.clone(), |bin| bin.join(target)),
        );
    }
    executables.extend(entry_program(&desktop_entry(&account.data_home, identity)));
    let install_dir = account
        .install_dir
        .clone()
        .unwrap_or_else(|| account.home.join(".local/lib"));
    executables.push(
        install_dir
            .join(&identity.directory_name)
            .join("bin")
            .join(EXECUTABLE),
    );
    let mut found = Vec::new();
    for executable in executables {
        let Ok(executable) = std::fs::canonicalize(&executable) else {
            continue;
        };
        if let Some(Location::Tree(root)) = tree_of(&executable, identity, account)
            && !found.contains(&root)
        {
            found.push(root);
        }
    }
    found.into_iter().map(Location::Tree).collect()
}

/// `~/.local/bin/<tree name>-desktop`, such as `spaceterm-preflight-desktop`.
fn launcher(home: &Path, name: &str) -> PathBuf {
    home.join(".local/bin").join(format!("{name}-desktop"))
}

fn desktop_entry(data_home: &Path, identity: &AppIdentity) -> PathBuf {
    data_home
        .join("applications")
        .join(format!("{}.desktop", identity.application_id))
}

fn icon(data_home: &Path, identity: &AppIdentity) -> PathBuf {
    data_home
        .join("icons/hicolor/scalable/apps")
        .join(format!("{}.svg", identity.application_id))
}

/// The program a desktop entry's `Exec` key runs.
fn entry_program(entry: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(entry).ok()?;
    let mut main = false;
    for line in text.lines() {
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
        } else if main && let Some(value) = line.strip_prefix("Exec=") {
            return exec_program(value).map(PathBuf::from);
        }
    }
    None
}

/// The first argument of an `Exec` value: its string escapes undone, then its quoting.
fn exec_program(value: &str) -> Option<String> {
    let mut unescaped = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next()? {
            's' => unescaped.push(' '),
            'n' => unescaped.push('\n'),
            't' => unescaped.push('\t'),
            'r' => unescaped.push('\r'),
            '\\' => unescaped.push('\\'),
            other => {
                unescaped.push('\\');
                unescaped.push(other);
            }
        }
    }
    let Some(quoted) = unescaped.strip_prefix('"') else {
        return unescaped.split(' ').next().map(str::to_owned);
    };
    let mut program = String::new();
    let mut characters = quoted.chars();
    loop {
        match characters.next()? {
            '"' => return Some(program),
            '\\' => program.push(characters.next()?),
            character => program.push(character),
        }
    }
}

/// Removes the tree and the launcher, icon and desktop entry that point into it.
fn remove_tree(root: &Path, identity: &AppIdentity, account: &Account) -> io::Result<()> {
    let executable = root.join("bin").join(EXECUTABLE);
    let launcher = launcher(&account.home, &identity.directory_name);
    if std::fs::read_link(&launcher).is_ok_and(|target| target == executable) {
        std::fs::remove_file(&launcher)?;
    }
    let icon = icon(&account.data_home, identity);
    if std::fs::read_link(&icon).is_ok_and(|target| target.starts_with(root)) {
        std::fs::remove_file(&icon)?;
    }
    let entry = desktop_entry(&account.data_home, identity);
    if entry_program(&entry).is_some_and(|program| program == executable) {
        remove_file(&entry)?;
    }
    if let (Some(parent), Some(name)) = (root.parent(), root.file_name()) {
        let mut staging = OsString::from(".");
        staging.push(name);
        staging.push(".update");
        remove_directory(&parent.join(staging))?;
    }
    remove_directory(root)
}

fn remove_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Removes a directory and everything in it, without following the symbolic links inside it. A
/// symbolic link in its place is refused.
pub fn remove_directory(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => Err(io::ErrorKind::InvalidInput.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "installed_app_tests.rs"]
mod tests;
