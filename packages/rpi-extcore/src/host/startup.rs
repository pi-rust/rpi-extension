//! Supplemental native-launch ownership and bounded startup diagnostics.
//! The native transcript lease remains authoritative. This separate stable
//! lock prevents a retry in the same registry from overtaking owned cleanup.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
};
#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;

const STDERR_LIMIT: usize = 16 * 1024;

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

pub(super) struct StderrCapture {
    tail: Arc<StdMutex<VecDeque<u8>>>,
    worker: tokio::task::JoinHandle<()>,
}
impl StderrCapture {
    pub(super) fn start(mut stderr: std::process::ChildStderr) -> Self {
        let tail = Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_LIMIT)));
        let output = tail.clone();
        // Both pipe reads and the inherited diagnostic sink can block. Keep
        // them off the async executor, including current-thread ACP runtimes.
        let worker = tokio::task::spawn_blocking(move || {
            let mut chunk = [0; 4096];
            loop {
                let count = match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                {
                    let mut tail = output.lock().expect("native stderr tail");
                    let excess = (tail.len() + count).saturating_sub(STDERR_LIMIT);
                    tail.drain(..excess);
                    tail.extend(&chunk[..count]);
                }
                // Preserve inherited-stderr behavior after ready as well. The
                // collector drains continuously, never buffering whole output.
                let _ = std::io::stderr().lock().write_all(&chunk[..count]);
            }
        });
        Self { tail, worker }
    }

    pub(super) async fn failure(mut self, error: String, child: &child::NativeChild) -> String {
        let cleanup = child.stop_checked().await;
        // The group has been stopped; give the drainer a bounded opportunity to
        // consume its final bytes. An inherited pipe is not exit evidence.
        // Do not infer exit from stderr EOF or abort a retained blocking
        // drainer. After this wait budget it still owns its pipe until EOF.
        let _ = tokio::time::timeout(Duration::from_millis(200), &mut self.worker).await;
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
