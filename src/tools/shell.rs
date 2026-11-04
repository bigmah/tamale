//! The `bash` tool's process handling. Commands run in their own process group, so a
//! timeout or an interrupt stops everything they started (not just `sh`), and stdout and
//! stderr share one pipe so their lines stay in order.

use std::time::Duration;

use anyhow::{Result, anyhow};

/// How long to wait for output after `sh` exits. Longer means a background process
/// started by the command still holds the pipe; it keeps running, uncaptured.
const EXIT_GRACE: Duration = Duration::from_millis(500);
/// Between asking a timed-out command to stop (SIGTERM) and killing it (SIGKILL).
const TERM_GRACE: Duration = Duration::from_secs(2);

/// `sh -c command`, without tamale's own secrets.
fn shell(command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::secure::withhold_secrets(&mut cmd);
    cmd
}

#[cfg(unix)]
pub async fn run(command: &str, timeout: Duration) -> Result<String> {
    use std::os::fd::OwnedFd;
    use tokio::io::AsyncReadExt;
    use tokio::net::unix::pipe;

    let (reader, writer) = std::io::pipe()?;
    let child = {
        // The command holds the write ends; it must be dropped once the child is spawned
        // so the pipe reaches EOF when the command's processes exit.
        let mut cmd = shell(command);
        cmd.stdout(writer.try_clone()?)
            .stderr(writer)
            .process_group(0);
        cmd.spawn()?
    };
    let mut child = child;
    let mut group = Group(child.id());
    let mut reader = pipe::Receiver::from_owned_fd(OwnedFd::from(reader))?;

    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut status = None;
    let mut eof = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut grace = Box::pin(tokio::time::sleep(Duration::MAX));
    let mut detached = false;

    while !(eof && status.is_some()) {
        tokio::select! {
            read = reader.read(&mut chunk), if !eof => match read {
                Ok(0) | Err(_) => eof = true,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            },
            exited = child.wait(), if status.is_none() => {
                status = Some(exited?);
                grace.as_mut().reset(tokio::time::Instant::now() + EXIT_GRACE);
            }
            () = &mut grace, if status.is_some() => {
                detached = true;
                break;
            }
            () = &mut deadline => {
                group.stop(&mut child).await;
                // Whatever is already in the pipe.
                while let Ok(Ok(n)) =
                    tokio::time::timeout(Duration::from_millis(50), reader.read(&mut chunk)).await
                {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut text = String::from_utf8_lossy(&buf).into_owned();
                push_line(
                    &mut text,
                    &format!(
                        "[timed out after {}s; the command and the processes it started were \
stopped. Output above is what it printed until then. Use a larger timeout_secs, or run \
long-lived processes in the background with output redirected to a file]",
                        timeout.as_secs()
                    ),
                );
                return Err(anyhow!(super::truncate(text)));
            }
        }
    }
    // Finished: whatever it left running in the background is on purpose.
    group.disarm();

    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if detached {
        push_line(
            &mut text,
            "[background processes started by the command are still running; their further \
output isn't captured]",
        );
    }
    let status = status.expect("loop ends after exit");
    let code = exit_code(&status);
    text.push_str(&format!("\n[exit code: {code}]"));
    Ok(super::truncate(text))
}

#[cfg(unix)]
fn exit_code(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code.to_string(),
        (None, Some(signal)) => format!("signal {signal}"),
        (None, None) => "signal".into(),
    }
}

fn push_line(text: &mut String, line: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
}

/// The command's process group, killed when dropped (an interrupted turn) unless disarmed.
#[cfg(unix)]
struct Group(Option<u32>);

#[cfg(unix)]
impl Group {
    fn signal(&self, signal: libc::c_int) {
        if let Some(pid) = self.0 {
            // SAFETY: killpg has no memory-safety preconditions. The group id is the
            // child's pid (`process_group(0)`).
            unsafe {
                libc::killpg(pid as libc::pid_t, signal);
            }
        }
    }

    /// Asks the group to stop, then kills what's left.
    async fn stop(&mut self, child: &mut tokio::process::Child) {
        self.signal(libc::SIGTERM);
        let _ = tokio::time::timeout(TERM_GRACE, child.wait()).await;
        self.signal(libc::SIGKILL);
        let _ = child.wait().await;
        self.0 = None;
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        self.signal(libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub async fn run(command: &str, timeout: Duration) -> Result<String> {
    let child = shell(command).output();
    let out = tokio::time::timeout(timeout, child)
        .await
        .map_err(|_| anyhow!("command timed out after {}s", timeout.as_secs()))??;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.is_empty() {
        push_line(&mut text, &stderr);
    }
    let code = out.status.code().map_or("signal".into(), |c| c.to_string());
    text.push_str(&format!("\n[exit code: {code}]"));
    Ok(super::truncate(text))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn commands_dont_get_the_hub_key() {
        let command = shell("true");
        let envs: Vec<_> = command.as_std().get_envs().collect();
        assert!(envs.contains(&(std::ffi::OsStr::new("TAMALE_HUB_KEY"), None)));
    }

    #[tokio::test]
    async fn interleaves_stdout_and_stderr_in_order() {
        let out = run("echo one; echo two >&2; echo three", Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(out, "one\ntwo\nthree\n\n[exit code: 0]");
    }

    #[tokio::test]
    async fn reports_exit_codes() {
        let out = run("echo oops >&2; exit 3", Duration::from_secs(10)).await.unwrap();
        assert_eq!(out, "oops\n\n[exit code: 3]");
    }

    #[tokio::test]
    async fn timeout_keeps_partial_output_and_kills_child_processes() {
        let marker = std::env::temp_dir().join(format!("tamale-bash-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let command = format!(
            "echo started; (sleep 3; touch {}) & sleep 30",
            marker.display()
        );
        let start = Instant::now();
        let err = run(&command, Duration::from_secs(1)).await.unwrap_err().to_string();
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert!(err.starts_with("started\n[timed out after 1s"), "{err}");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "background child survived the timeout");
    }

    #[tokio::test]
    async fn dropping_the_call_kills_the_process_group() {
        let marker = std::env::temp_dir().join(format!("tamale-bash-drop-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let command = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let call = tokio::spawn(async move { run(&command, Duration::from_secs(60)).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        call.abort();
        let _ = call.await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "child survived the interrupt");
    }

    #[tokio::test]
    async fn background_processes_dont_block_the_result() {
        let start = Instant::now();
        let out = run("echo hi; sleep 5 &", Duration::from_secs(30)).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
        assert!(out.starts_with("hi\n[background processes"), "{out}");
        assert!(out.ends_with("[exit code: 0]"), "{out}");
    }
}
