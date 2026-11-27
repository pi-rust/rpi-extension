//! Owned process-tree lifetime. The child is guarded before fallible setup.
//!
//! Process-group cleanup is adapted from the MIT-licensed rpi-plugin-sdk.
//! Copyright (c) 2026 BigFish.
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.
//!
use std::io;
use std::process::{Child, Command};

#[cfg(unix)]
use nix::{
    sys::signal::{killpg, Signal},
    unistd::Pid,
};
#[cfg(windows)]
use rpi_plugin_sdk::process::ProcessTree as WindowsProcessTree;

mod file_lock;
pub use file_lock::SharedFileLock;

/// Contained process tree owned by the component that spawns it. The command,
/// executable, arguments and environment must come from the caller's authorized
/// configuration; this type does not authorize untrusted remote command input.
/// Stop/Drop target only the owned process group/job, never a caller-supplied PID.
/// `stop` includes synchronous OS termination/wait and must run in a retained
/// blocking cleanup owner when used from async code. Kernel I/O/wait failures are
/// not hard-preemptible deadlines. Taking pipes is supported; detaching/replacing
/// the owned Child or deliberately escaping containment violates this contract.
#[derive(Debug)]
pub struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    tree: Option<WindowsProcessTree>,
    #[cfg(unix)]
    group: Pid,
    stopped: bool,
}

impl ProcessTree {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Self::spawn_with_attach(command, |child| {
                WindowsProcessTree::attach(child).map_err(io::Error::other)
            })
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let child = command.spawn()?;
            Ok(Self {
                #[cfg(unix)]
                group: Pid::from_raw(child.id() as i32),
                child,
                stopped: false,
            })
        }
    }

    #[cfg(windows)]
    fn spawn_with_attach(
        command: &mut Command,
        attach: impl FnOnce(&Child) -> io::Result<WindowsProcessTree>,
    ) -> io::Result<Self> {
        WindowsProcessTree::prepare(command);
        let mut owner = Self {
            child: command.spawn()?,
            tree: None,
            stopped: false,
        };
        owner.tree = Some(attach(&owner.child)?);
        Ok(owner)
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        // Do not gate tree termination on Child::try_wait: a dead leader can
        // leave live descendants holding all transport pipes. The flag also
        // prevents a later Drop from signaling a potentially recycled Unix PGID.
        self.stopped = true;
        #[cfg(windows)]
        if let Some(tree) = self.tree.take() {
            tree.kill();
            // Dropping the job also kills it before we wait for the child.
        }
        #[cfg(unix)]
        let _ = killpg(self.group, Signal::SIGKILL);
        // Also handles Windows errors before job assignment and tree-kill
        // failures. Child::kill/::wait retain the actual process handle/PID.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct Canary(PathBuf);
    impl Canary {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            Self(std::env::temp_dir().join(format!(
                "mcp-process-canary-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn command(&self) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "process::tests::child_canary", "--nocapture"])
                .env("RPI_MCP_PROCESS_TEST_CANARY", &self.0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        }
    }
    impl Drop for Canary {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn child_canary() {
        if let Some(path) = std::env::var_os("RPI_MCP_PROCESS_TEST_CANARY") {
            std::fs::write(path, "server code executed").unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    fn wait_started(canary: &Canary) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !canary.0.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not execute startup canary"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(windows)]
    struct Probe {
        pid: u32,
        // Keep the process object alive so its PID cannot be reused.
        _handle: std::os::windows::io::OwnedHandle,
    }
    #[cfg(windows)]
    impl Probe {
        fn new(child: &Child) -> Self {
            use std::os::windows::io::AsHandle;
            Self {
                pid: child.id(),
                _handle: child.as_handle().try_clone_to_owned().unwrap(),
            }
        }
        fn assert_exited(&self) {
            assert!(!rpi_plugin_sdk::process::process_is_running(self.pid).unwrap());
        }
    }
    #[cfg(unix)]
    struct Probe(Pid);
    #[cfg(unix)]
    impl Probe {
        fn new(child: &Child) -> Self {
            Self(Pid::from_raw(child.id() as i32))
        }
        fn assert_exited(&self) {
            use nix::sys::wait::{waitpid, WaitPidFlag};
            // ECHILD proves the owner already reaped it.
            assert_eq!(
                waitpid(self.0, Some(WaitPidFlag::WNOHANG)),
                Err(nix::errno::Errno::ECHILD)
            );
        }
    }

    #[test]
    fn owner_drop_kills_and_waits_real_child() {
        let canary = Canary::new();
        let mut owner = ProcessTree::spawn(&mut canary.command()).unwrap();
        let probe = Probe::new(owner.child_mut());
        wait_started(&canary);
        drop(owner);
        probe.assert_exited();
    }

    #[test]
    fn post_spawn_pipe_setup_error_kills_and_waits_real_child() {
        let canary = Canary::new();
        let mut probe = None;
        let result = (|| -> io::Result<()> {
            // stdout is deliberately not piped: exercise the same fallible
            // extraction as transport, while the newly spawned owner is live.
            let mut owner = ProcessTree::spawn(&mut canary.command())?;
            probe = Some(Probe::new(owner.child_mut()));
            wait_started(&canary);
            let _stdout = owner
                .child_mut()
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("stdout unavailable"))?;
            Ok(())
        })();
        assert!(result.is_err());
        probe.unwrap().assert_exited();
    }

    #[test]
    fn stop_is_idempotent_and_reaps_real_child() {
        let canary = Canary::new();
        let mut owner = ProcessTree::spawn(&mut canary.command()).unwrap();
        let probe = Probe::new(owner.child_mut());
        wait_started(&canary);
        owner.stop();
        assert!(owner.stopped);
        assert!(owner.child_mut().try_wait().unwrap().is_some());
        owner.stop();
        drop(owner);
        probe.assert_exited();
    }

    #[cfg(windows)]
    #[test]
    fn failed_attach_kills_and_waits_suspended_child() {
        let canary = Canary::new();
        let mut probe = None;
        let result = ProcessTree::spawn_with_attach(&mut canary.command(), |child| {
            probe = Some(Probe::new(child));
            Err(io::Error::other("injected process-tree attach error"))
        });
        assert_eq!(
            result.unwrap_err().to_string(),
            "injected process-tree attach error"
        );
        probe
            .expect("child spawned before attachment")
            .assert_exited();
        assert!(
            !canary.0.exists(),
            "suspended child executed before attachment"
        );
    }
}
