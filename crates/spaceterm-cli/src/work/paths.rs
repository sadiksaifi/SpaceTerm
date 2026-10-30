//! Path authority comes from the selected route. Snapshot paths never authorize local I/O.
use crate::contract::Code;
use crate::scope::{ScopeArgs, Selector};
use std::path::{Path, PathBuf};

pub struct Paths<'a> {
    pub local: bool,
    /// The CLI process's working directory. Ignored for paired routes.
    pub cwd: &'a Path,
    /// The selected server account's home, obtained from that server.
    pub home: &'a Path,
}

impl Paths<'_> {
    /// Validate before dispatch. A remote relative path is always a usage failure.
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, Code> {
        if path.as_os_str().is_empty() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(Code::InvalidArguments);
        }
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        if let Ok(suffix) = path.strip_prefix("~") {
            // Only the literal ~/ form expands; a bare ~ and ~user are relative paths.
            if path.as_os_str().as_encoded_bytes().starts_with(b"~/") {
                return Ok(self.home.join(suffix));
            }
        }
        if self.local {
            Ok(self.cwd.join(path))
        } else {
            Err(Code::InvalidArguments)
        }
    }

    pub fn scope(&self, args: &ScopeArgs) -> Result<ScopeArgs, Code> {
        let mut args = args.clone();
        for (index, selector) in [
            &mut args.workspace,
            &mut args.worktree,
            &mut args.tab,
            &mut args.pane,
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(Selector::Path(path)) = selector {
                if index != 1 {
                    return Err(Code::InvalidArguments);
                }
                *path = self.resolve(path)?;
            }
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_resolve_against_the_invoking_process_and_account() {
        let paths = Paths {
            local: true,
            cwd: Path::new("/checkout"),
            home: Path::new("/account"),
        };
        assert_eq!(
            paths.resolve(Path::new("child")).unwrap(),
            Path::new("/checkout/child")
        );
        assert_eq!(
            paths.resolve(Path::new("~/child")).unwrap(),
            Path::new("/account/child")
        );
        assert_eq!(
            paths.resolve(Path::new("/absolute")).unwrap(),
            Path::new("/absolute")
        );
    }

    #[test]
    fn remote_relative_paths_fail_without_exposing_the_value() {
        let paths = Paths {
            local: false,
            cwd: Path::new("/client"),
            home: Path::new("/server/home"),
        };
        for path in ["private/secret", "~", "~other/repo", ""] {
            let error = paths.resolve(Path::new(path)).unwrap_err();
            assert_eq!(error, Code::InvalidArguments);
        }
        assert_eq!(
            paths.resolve(Path::new("~/repo")).unwrap(),
            Path::new("/server/home/repo")
        );
    }
}
