//! `tamale update`: replaces this tamale with the latest release, using the installer that
//! `dist` publishes with each one (the same one `install.sh` runs).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const REPO: &str = "bigmah/tamale";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

pub async fn run() -> Result<()> {
    if !cfg!(unix) {
        bail!(
            "`tamale update` works on macOS and Linux; download the latest release from \
             https://github.com/{REPO}/releases/latest"
        );
    }
    let exe = std::env::current_exe().context("couldn't find the running tamale")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    // The installer puts tamale in `<dir>/bin`; anything else wasn't installed by it.
    let Some(install_dir) = install_dir(&exe) else {
        bail!(
            "This tamale ({}) wasn't installed by the tamale installer, so `tamale update` \
             can't replace it. Rebuild it from source, or install a release with:\n  \
             curl --proto '=https' --tlsv1.2 -LsSf \
             https://github.com/{REPO}/releases/latest/download/install.sh | sh",
            exe.display()
        );
    };

    let client = reqwest::Client::builder()
        .user_agent(concat!("tamale/", env!("CARGO_PKG_VERSION")))
        .build()?;
    println!("Checking for a newer tamale (this is {CURRENT})…");
    let release: Release = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("couldn't reach GitHub to find the latest release")?
        .json()
        .await
        .context("couldn't read the latest release from GitHub")?;
    let latest = release.tag_name.trim_start_matches('v');
    if !newer(latest, CURRENT) {
        println!("tamale {CURRENT} is the latest version.");
        return Ok(());
    }

    println!("Installing tamale {latest}…");
    let script = client
        .get(format!(
            "https://github.com/{REPO}/releases/download/{}/tamale-installer.sh",
            release.tag_name
        ))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("couldn't download the installer")?
        .bytes()
        .await
        .context("couldn't download the installer")?;
    let path = std::env::temp_dir().join(format!("tamale-installer-{}.sh", std::process::id()));
    std::fs::write(&path, &script).context("couldn't save the installer")?;
    let status = tokio::process::Command::new("sh")
        .arg(&path)
        .arg("--quiet")
        .env("TAMALE_INSTALL_DIR", &install_dir)
        // PATH was set up by the first install; an update leaves shell profiles alone.
        .env("TAMALE_NO_MODIFY_PATH", "1")
        .stdin(std::process::Stdio::null())
        .status()
        .await;
    let _ = std::fs::remove_file(&path);
    let status = status.context("couldn't run the installer")?;
    if !status.success() {
        bail!("the installer failed ({status})");
    }
    println!("Updated tamale {CURRENT} → {latest} ({}).", exe.display());
    #[cfg(unix)]
    {
        let running = crate::background::list().len();
        if running > 0 {
            let (sessions, keep) = if running == 1 {
                ("session", "keeps")
            } else {
                ("sessions", "keep")
            };
            println!(
                "{running} {sessions} already running {keep} {CURRENT} until they end \
                 (`tamale ls` lists them)."
            );
        }
    }
    Ok(())
}

/// The directory the installer was given for `exe`, which it puts in `<dir>/bin`.
fn install_dir(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    (bin.file_name()? == "bin").then(|| bin.parent().map(Path::to_path_buf))?
}

/// Whether version `a` is newer than `b`, comparing dotted numbers (a pre-release suffix,
/// like `-beta.1`, is ignored).
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|n| n.parse().unwrap_or(0))
            .collect()
    };
    parts(a) > parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("0.2.0", "0.1.7"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.1.7", "0.1.7"));
        assert!(!newer("0.1.6", "0.1.7"));
    }

    #[test]
    fn installs_next_to_the_running_binary() {
        assert_eq!(
            install_dir(Path::new("/home/me/.cargo/bin/tamale")),
            Some(PathBuf::from("/home/me/.cargo"))
        );
        assert_eq!(
            install_dir(Path::new("/opt/tamale/bin/tamale")),
            Some(PathBuf::from("/opt/tamale"))
        );
        assert_eq!(
            install_dir(Path::new("/src/tamale/target/release/tamale")),
            None
        );
    }
}
