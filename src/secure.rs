//! Small protections used across tamale: what the programs it starts don't get, who can read
//! what it keeps on disk, and what reaches a terminal as is.

use std::borrow::Cow;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

/// tamale's own secrets. Programs it starts for the agent don't need them, and a key that isn't
/// in their environment can't end up in a transcript through `env`, a test's log or a build
/// script that reports what it sees. This doesn't stop a determined prompt injection: tools run
/// as you, and can read your shell profile (see `docs/remote-access.md`).
const OWN_SECRETS: [&str; 2] = ["TAMALE_HUB_KEY", "TAMALE_HUB_TOKEN"];

pub fn withhold_secrets(command: &mut tokio::process::Command) {
    for name in OWN_SECRETS {
        command.env_remove(name);
    }
}

/// Makes `dir` and its parents if they aren't there, and `dir` reachable only by this user.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Writes `bytes` to `path`, a new file only this user can read. A file already there keeps its
/// mode, so write to a temporary name and rename, as the callers do, or to a private directory.
pub fn write_private(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    private_file()
        .write(true)
        .truncate(true)
        .open(path)?
        .write_all(bytes.as_ref())
}

/// Appends `bytes` to `path`, made if need be as [`write_private`] does.
pub fn append_private(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    private_file()
        .append(true)
        .open(path)?
        .write_all(bytes.as_ref())
}

/// Whether two secrets are equal, in time that depends only on their lengths.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// `text` without what a terminal would act on rather than show. Model output and file names
/// can hold escape sequences, which can set the title, write the clipboard or redraw what's on
/// screen; without their control characters they're just text. Newlines and tabs stay.
pub fn printable(text: &str) -> Cow<'_, str> {
    let acts = |c: char| c.is_control() && c != '\n' && c != '\t';
    if text.contains(acts) {
        Cow::Owned(text.chars().filter(|c| !acts(*c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_dont_get_tamales_secrets() {
        let mut command = tokio::process::Command::new("true");
        withhold_secrets(&mut command);
        let envs: Vec<_> = command.as_std().get_envs().collect();
        for name in OWN_SECRETS {
            assert!(envs.contains(&(std::ffi::OsStr::new(name), None)), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_files_and_directories_are_this_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let root = std::env::temp_dir().join(format!("tamale-secure-{}", crate::session::new_id()));
        let dir = root.join("nested/dir");
        private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        // One that was there, more open, is closed up too.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        let file = dir.join("file");
        write_private(&file, "one").unwrap();
        assert_eq!(mode(&file), 0o600);
        append_private(&file, " two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one two");
        let appended = dir.join("appended");
        append_private(&appended, "x").unwrap();
        assert_eq!(mode(&appended), 0o600);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn secrets_compare_by_value() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "ab"));
        assert!(same_secret("", ""));
    }

    #[test]
    fn terminals_get_no_control_characters() {
        assert!(matches!(printable("plain\ttext\nüñí"), Cow::Borrowed(_)));
        // Writing the clipboard (OSC 52), and a C1 control that starts a sequence on its own.
        assert_eq!(
            printable("a\u{1b}]52;c;aGk=\u{7}b\u{9b}2Jc\r\n"),
            "a]52;c;aGk=b2Jc\n"
        );
    }
}
