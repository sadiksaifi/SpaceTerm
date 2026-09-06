use crate::observation::{AcceptanceObservationError, PrivateEnvironment};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
pub(crate) struct PrivateDirectories;
impl PrivateEnvironment for PrivateDirectories {
    fn validate_directory(&self, path: &Path) -> Result<PathBuf, AcceptanceObservationError> {
        let invalid = || AcceptanceObservationError::InvalidEnvironment;
        let source = path.symlink_metadata().map_err(|_| invalid())?;
        if source.file_type().is_symlink() {
            return Err(invalid());
        }
        let path = path.canonicalize().map_err(|_| invalid())?;
        let metadata = path.symlink_metadata().map_err(|_| invalid())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(invalid());
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::clean_acceptance_environment;
    use std::{
        env,
        ffi::OsString,
        os::unix::fs::PermissionsExt,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };
    #[test]
    fn clean_environment_should_isolate_real_zsh_and_bash_from_hostile_startup_overrides() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "spaceterm-acceptance-environment-{}-{unique}",
            std::process::id()
        ));
        let home = root.join("home");
        let config = home.join(".xdg/config");
        let data = home.join(".xdg/data");
        let state = home.join(".xdg/state");
        let cache = home.join(".xdg/cache");
        let hostile = root.join("permanent-sentinel");
        for path in [&home, &config, &data, &state, &cache, &hostile] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let marker = hostile.join("startup-was-read");
        std::fs::write(
            hostile.join(".zshenv"),
            format!(
                "print -r -- PERMANENT_SENTINEL\nprint -r -- TRUST_PROMPT\n: > {}\n",
                marker.display()
            ),
        )
        .unwrap();
        let bash_environment = hostile.join("bash-env");
        std::fs::write(
            &bash_environment,
            format!(
                "printf 'PERMANENT_SENTINEL\\nTRUST_PROMPT\\n'\n: > {}\n",
                marker.display()
            ),
        )
        .unwrap();

        let variables = [
            ("USER", "fixture-user".to_owned()),
            ("LOGNAME", "fixture-user".to_owned()),
            ("SHELL", "/bin/zsh".to_owned()),
            ("PATH", "/usr/bin:/bin".to_owned()),
            ("LANG", "C".to_owned()),
            ("LC_ALL", "C".to_owned()),
            ("TMPDIR", root.to_string_lossy().into_owned()),
            ("HOME", home.to_string_lossy().into_owned()),
            ("XDG_CONFIG_HOME", config.to_string_lossy().into_owned()),
            ("XDG_DATA_HOME", data.to_string_lossy().into_owned()),
            ("XDG_STATE_HOME", state.to_string_lossy().into_owned()),
            ("XDG_CACHE_HOME", cache.to_string_lossy().into_owned()),
            ("ZDOTDIR", hostile.to_string_lossy().into_owned()),
            ("BASH_ENV", bash_environment.to_string_lossy().into_owned()),
            ("ENV", bash_environment.to_string_lossy().into_owned()),
            ("INPUTRC", bash_environment.to_string_lossy().into_owned()),
            ("HISTFILE", marker.to_string_lossy().into_owned()),
            ("MISE_CONFIG_DIR", hostile.to_string_lossy().into_owned()),
            (
                "MISE_TRUSTED_CONFIG_PATHS",
                hostile.to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));
        let clean = clean_acceptance_environment(&PrivateDirectories, variables).unwrap();

        let zsh = Command::new("/bin/zsh")
            .arg("-c")
            .arg("printf ZSH_CLEAN; : > \"$XDG_STATE_HOME/zsh-write\"")
            .env_clear()
            .envs(&clean)
            .output()
            .unwrap();
        let bash = Command::new("/bin/bash")
            .arg("-c")
            .arg("printf BASH_CLEAN; : > \"$XDG_STATE_HOME/bash-write\"")
            .env_clear()
            .envs(&clean)
            .output()
            .unwrap();
        assert_eq!(zsh.stdout, b"ZSH_CLEAN");
        assert!(zsh.stderr.is_empty());
        assert_eq!(bash.stdout, b"BASH_CLEAN");
        assert!(bash.stderr.is_empty());
        assert!(!marker.exists());
        assert!(state.join("zsh-write").is_file());
        assert!(state.join("bash-write").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
