//! The working directory and additional directories included in the agent's task.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub cwd: PathBuf,
    pub additional_dirs: Vec<PathBuf>,
}

impl Workspace {
    pub fn current() -> Result<Self> {
        Ok(Self {
            cwd: std::env::current_dir()?,
            additional_dirs: Vec::new(),
        })
    }

    /// Resolve against cwd, expand ~/ and deduplicate canonical paths (including symlinks).
    /// Returns false if the directory is already included.
    pub fn add_dir(&mut self, path: &Path) -> Result<bool> {
        ensure!(
            !path.as_os_str().is_empty(),
            "directory path must not be empty"
        );
        let expanded = if path == Path::new("~") || path.starts_with("~/") {
            let home = std::env::var_os("HOME").context("HOME is not set; use an absolute path")?;
            PathBuf::from(home).join(path.strip_prefix("~")?)
        } else {
            self.cwd.join(path)
        };
        let resolved = expanded
            .canonicalize()
            .with_context(|| format!("opening directory {}", expanded.display()))?;
        ensure!(
            resolved.is_dir(),
            "{} is not a directory",
            resolved.display()
        );
        let cwd = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
        if resolved == cwd || self.additional_dirs.contains(&resolved) {
            return Ok(false);
        }
        self.additional_dirs.push(resolved);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_paths_and_rejects_files_and_missing_directories_without_changing_scope() {
        let root =
            std::env::temp_dir().join(format!("tamale-workspace-{}", crate::session::new_id()));
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::create_dir_all(root.join("shared files")).unwrap();
        std::fs::write(root.join("file"), "text").unwrap();
        let mut workspace = Workspace {
            cwd: root.join("project"),
            additional_dirs: Vec::new(),
        };
        assert!(!workspace.add_dir(Path::new(".")).unwrap());
        assert!(workspace.add_dir(Path::new("../shared files")).unwrap());
        let canonical = root.join("shared files").canonicalize().unwrap();
        assert_eq!(workspace.additional_dirs, [canonical.clone()]);
        assert!(!workspace.add_dir(&canonical).unwrap());
        assert!(
            !workspace
                .add_dir(Path::new("../shared files/../shared files"))
                .unwrap()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&canonical, root.join("alias")).unwrap();
            assert!(!workspace.add_dir(Path::new("../alias")).unwrap());
        }
        for path in ["", "../file", "../missing"] {
            assert!(workspace.add_dir(Path::new(path)).is_err());
        }
        assert_eq!(workspace.additional_dirs, [canonical]);
        assert_eq!(workspace.cwd, root.join("project"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expands_home_without_changing_the_process_directory() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let mut workspace = Workspace::current().unwrap();
        let cwd = workspace.cwd.clone();
        workspace.add_dir(Path::new("~")).unwrap();
        let expected = PathBuf::from(home).canonicalize().unwrap();
        assert!(
            workspace.additional_dirs.contains(&expected)
                || cwd.canonicalize().unwrap() == expected
        );
        assert!(!workspace.add_dir(Path::new("~/.")).unwrap());
        assert_eq!(std::env::current_dir().unwrap(), cwd);
    }
}
