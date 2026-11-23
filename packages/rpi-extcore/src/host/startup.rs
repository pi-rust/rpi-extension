//! Supplemental native-launch ownership and bounded startup diagnostics.
//! The native transcript lease remains authoritative. This separate stable
//! lock prevents a retry in the same registry from overtaking owned cleanup.
use super::*;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Write};
#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;

const STDERR_LIMIT: usize = 16 * 1024;
const STDERR_DRAIN_GRACE: Duration = Duration::from_millis(200);

pub(super) struct LaunchGuard {
    _file: File,
}
impl LaunchGuard {
    pub(super) fn acquire(options: &SessionOptions) -> Result<Self, String> {
        std::fs::create_dir_all(&options.timings_dir)
            .map_err(|e| format!("cannot create native launch ownership directory: {e}"))?;
        // Hash untrusted IDs rather than interpreting them as path components.
        // Registry session IDs are unique, including provisional fork IDs.
        let name = format!(
            "native-owner-{:x}.lock",
            Sha256::digest(options.session_id.as_bytes())
        );
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(options.timings_dir.join(name))
            .map_err(|e| format!("cannot open native launch ownership lock: {e}"))?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            {
                "native session is already owned or its cleanup is still in progress".to_string()
            } else {
                format!("cannot acquire native launch ownership lock: {e}")
            }
        })?;
        // Never unlink this file: replacing its inode would split ownership.
        Ok(Self { _file: file })
    }
}

// Tokio wraps standard Windows child pipes in blocking-pool reads, so merely
// using ChildStderr::from_std would leave the same uncancellable-read problem.
// The server is overlapped/IOCP-backed; the child inherits only its write end.
#[cfg(windows)]
pub(super) async fn windows_stderr_pipe(
) -> std::io::Result<(File, tokio::net::windows::named_pipe::NamedPipeServer)> {
    let name = format!(r"\\.\pipe\rpi-native-stderr-{}", uuid::Uuid::new_v4());
    let reader = tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(true)
        .max_instances(1)
        .access_inbound(true)
        .access_outbound(false)
        .reject_remote_clients(true)
        .create(&name)?;
    // This connects only to the private local server just created above;
    // no child exists yet and no diagnostic data is read synchronously.
    let writer = std::fs::OpenOptions::new().write(true).open(name)?;
    reader.connect().await?;
    Ok((writer, reader))
}

pub(super) struct StderrCapture {
    tail: Arc<StdMutex<VecDeque<u8>>>,
    worker: tokio::task::JoinHandle<()>,
}
impl StderrCapture {
    pub(super) fn start(
        mut stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        mut exit: tokio::sync::watch::Receiver<Option<bool>>,
    ) -> Self {
        let tail = Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_LIMIT)));
        let output = tail.clone();
        // A descendant can escape the owned process group and retain stderr
        // after the native leader exits. Pipe EOF is not a bounded lifetime:
        // keep the read cancellable, independently of OS-reap/launch ownership.
        let worker = tokio::spawn(async move {
            let drain = async {
                let mut chunk = [0; 4096];
                loop {
                    let count = match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => count,
                    };
                    {
                        let mut tail = output.lock().expect("native stderr tail");
                        let excess = (tail.len() + count).saturating_sub(STDERR_LIMIT);
                        tail.drain(..excess);
                        tail.extend(&chunk[..count]);
                    }
                    // Preserve forwarding without blocking current-thread ACP
                    // executors. Only the bounded chunk, never the child pipe,
                    // is owned by the blocking diagnostic-sink write.
                    let bytes = chunk[..count].to_vec();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = std::io::stderr().lock().write_all(&bytes);
                    })
                    .await;
                }
            };
            tokio::pin!(drain);
            tokio::select! {
                _ = &mut drain => {}
                _ = async {
                    // Drop the watch Ref before the drain grace awaits.
                    let _ = exit.wait_for(|state| *state == Some(true)).await;
                } => {
                    // Allow final diagnostics, but do not let an inherited
                    // descriptor retain the reader indefinitely. A closed
                    // exit channel also retires diagnostics, not launch locks.
                    let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, &mut drain).await;
                }
            }
        });
        Self { tail, worker }
    }

    pub(super) async fn failure(mut self, error: String, child: &child::NativeChild) -> String {
        let cleanup = child.stop_checked().await;
        // Startup-failure reporting has its own drain budget even when child
        // exit is unconfirmed. Cancelling this async reader closes only the
        // diagnostic pipe; it cannot release the independent launch guard.
        if tokio::time::timeout(STDERR_DRAIN_GRACE, &mut self.worker)
            .await
            .is_err()
        {
            self.worker.abort();
            let _ = (&mut self.worker).await;
        }
        let bytes: Vec<u8> = self
            .tail
            .lock()
            .expect("native stderr tail")
            .iter()
            .copied()
            .collect();
        let diagnostic: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect();
        let mut message = format!("{error} ({})", child.description());
        if let Err(cleanup) = cleanup {
            message.push_str(&format!(
                "; cleanup not confirmed: {cleanup}; reopen remains blocked"
            ));
        }
        if !diagnostic.trim().is_empty() {
            message.push_str("; native stderr (bounded tail): ");
            message.push_str(diagnostic.trim());
        }
        message
    }
}
