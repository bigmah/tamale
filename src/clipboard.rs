//! System clipboard access through the platform's clipboard utilities.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub async fn read() -> Result<String> {
    let bytes = run(None).await?;
    String::from_utf8(bytes).context("clipboard does not contain UTF-8 text")
}

pub async fn write(text: &str) -> Result<()> {
    run(Some(text)).await?;
    Ok(())
}

async fn run(input: Option<&str>) -> Result<Vec<u8>> {
    let mut errors = Vec::new();
    for (program, args) in commands(input.is_some()) {
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let mut child = Command::new(program)
                .args(args)
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                // Linux clipboard owners may fork and keep their descriptors open.
                // Copy has no output to collect, so don't wait for inherited pipes.
                .stdout(if input.is_some() {
                    Stdio::null()
                } else {
                    Stdio::piped()
                })
                .stderr(if input.is_some() {
                    Stdio::null()
                } else {
                    Stdio::piped()
                })
                .kill_on_drop(true)
                .spawn()?;
            if let Some(text) = input {
                let mut stdin = child.stdin.take().context("clipboard stdin unavailable")?;
                stdin.write_all(text.as_bytes()).await?;
                // Closing stdin lets the clipboard utility finish reading.
                drop(stdin);
            }
            let output = child.wait_with_output().await?;
            if !output.status.success() {
                bail!(
                    "exit status {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(output.stdout)
        })
        .await;
        match result {
            Ok(Ok(bytes)) => return Ok(bytes),
            Ok(Err(error)) => errors.push(format!("{program}: {error}")),
            Err(_) => errors.push(format!("{program}: timed out")),
        }
    }
    bail!("system clipboard unavailable ({})", errors.join("; "))
}

fn commands(copy: bool) -> Vec<(&'static str, &'static [&'static str])> {
    #[cfg(target_os = "macos")]
    {
        if copy {
            vec![("pbcopy", &[])]
        } else {
            vec![("pbpaste", &[])]
        }
    }
    #[cfg(target_os = "windows")]
    {
        const COPY: &[&str] = &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::InputEncoding = [Text.UTF8Encoding]::new(); Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ];
        const PASTE: &[&str] = &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding = [Text.UTF8Encoding]::new(); [Console]::Write((Get-Clipboard -Raw))",
        ];
        vec![("powershell.exe", if copy { COPY } else { PASTE })]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if copy {
            vec![
                ("wl-copy", &[]),
                ("xclip", &["-selection", "clipboard", "-in"]),
                ("xsel", &["--clipboard", "--input"]),
            ]
        } else {
            vec![
                ("wl-paste", &["--no-newline"]),
                ("xclip", &["-selection", "clipboard", "-out"]),
                ("xsel", &["--clipboard", "--output"]),
            ]
        }
    }
}
