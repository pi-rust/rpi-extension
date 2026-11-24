//! Native child-tree ownership independent of RPC reader cancellation. Only
//! this retained blocking reaper owns the contained process tree. EOF, stop ACK
//! and an aborted reader are not OS exit evidence.
use super::*;
#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;

pub(super) struct NativeChild {
    stop: tokio::sync::watch::Sender<bool>,
    exit: tokio::sync::watch::Receiver<Option<bool>>,
    pid: u32,
    status: Arc<StdMutex<Option<std::process::ExitStatus>>>,
}
impl NativeChild {
    pub(super) fn start(
        mut tree: rpi_mcp_core::ProcessTree,
        launch_guard: Option<startup::LaunchGuard>,
    ) -> Self {
        let pid = tree.child_mut().id();
        let status = Arc::new(StdMutex::new(None));
        let observed = status.clone();
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let (exited, exit) = tokio::sync::watch::channel(None);
        // A started blocking task cannot be aborted with the reader. It owns
        // cleanup AND launch exclusion until confirmed OS exit, even when every
        // request/host future is dropped or a stop_checked waiter times out.
        tokio::task::spawn_blocking(move || {
            loop {
                if *stopping.borrow() || stopping.has_changed().is_err() {
                    break;
                }
                match tree.child_mut().try_wait() {
                    Ok(Some(status)) => {
                        *observed.lock().expect("native exit status") = Some(status);
                        break;
                    }
                    Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            // Includes owned group/job cleanup and leader wait.
            tree.stop();
            // Unknown exit must never release launch ownership. Retain the
            // reaper and retry OS observation, not native startup or RPC work.
            Self::confirm_exit(
                || tree.child_mut().try_wait(),
                launch_guard,
                exited,
                observed,
            );
        });
        Self {
            stop,
            exit,
            pid,
            status,
        }
    }
    fn confirm_exit(
        mut observe: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
        launch_guard: Option<startup::LaunchGuard>,
        exited: tokio::sync::watch::Sender<Option<bool>>,
        status: Arc<StdMutex<Option<std::process::ExitStatus>>>,
    ) {
        loop {
            match observe() {
                Ok(Some(observed)) => {
                    *status.lock().expect("native exit status") = Some(observed);
                    drop(launch_guard);
                    exited.send_replace(Some(true));
                    return;
                }
                _ => {
                    exited.send_replace(Some(false));
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    pub(super) fn description(&self) -> String {
        let status = self.status.lock().expect("native exit status");
        match status.as_ref() {
            Some(status) => format!("pid {}, {status}", self.pid),
            None => format!("pid {}, exit status unavailable", self.pid),
        }
    }
    pub(super) fn stop_signal(&self) -> tokio::sync::watch::Sender<bool> {
        self.stop.clone()
    }
    pub(super) fn exit_signal(&self) -> tokio::sync::watch::Receiver<Option<bool>> {
        self.exit.clone()
    }
    pub(super) async fn stop_checked(&self) -> Result<(), String> {
        self.stop.send_replace(true);
        Self::wait_checked(self.exit.clone()).await
    }
    pub(super) async fn wait_checked(
        mut exit: tokio::sync::watch::Receiver<Option<bool>>,
    ) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(10), async {
            if exit.borrow().is_none() {
                let _ = exit.wait_for(Option::is_some).await;
            }
            if *exit.borrow() == Some(true) {
                Ok(())
            } else {
                Err("owned native child OS exit not confirmed".into())
            }
        })
        .await
        .map_err(|_| "owned native child OS exit deadline exceeded".to_string())?
    }
}
impl Drop for NativeChild {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
