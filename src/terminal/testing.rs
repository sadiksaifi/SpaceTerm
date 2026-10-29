use std::path::PathBuf;

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
pub(crate) use desktop::*;

/// Isolated resource layout supplied explicitly to portable launch policy.
pub(crate) struct ShellResourcesFixture(PathBuf);
impl ShellResourcesFixture {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spaceterm-resources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        for relative in [
            "bash/spaceterm.bash",
            "elvish/lib/spaceterm-integration.elv",
            "elvish/rc.elv",
            "fish/vendor_conf.d/spaceterm-shell-integration.fish",
            "nushell/vendor/autoload/spaceterm.nu",
            "zsh/.zshenv",
        ] {
            let path = root.join("shell-integration").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        Self(root)
    }
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for ShellResourcesFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
