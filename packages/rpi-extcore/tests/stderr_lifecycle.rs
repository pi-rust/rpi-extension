//! Fixture-only lifecycle regressions: a daemon escapes the native process
//! group and keeps stderr open. All descendants are explicitly test-owned and
//! terminated before assertions, including on failure. No real-native/model use.
#![cfg(unix)]

use rpi_extcore::host::{HostSession, SessionOptions};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
enum Mode {
    Ready,
    Failure,
    Cancelled,
}

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(mode: Mode) -> Self {
        let root = std::env::temp_dir().join(format!("native-stderr-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let script = root.join("native");
        fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, pathlib, subprocess, sys, time
root = pathlib.Path(__file__).parent
# A finite safety deadline backs up explicit fixture cleanup. Only stderr is
# inherited, so the daemon cannot delay RPC stdout EOF or command writes.
daemon = 'import os,pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); time.sleep(40)'
subprocess.Popen([sys.executable, '-c', daemon, str(root / 'daemon.pid')],
                 start_new_session=True, stdin=subprocess.DEVNULL,
                 stdout=subprocess.DEVNULL)
while not (root / 'daemon.pid').exists(): time.sleep(0.005)
if (root / 'cancelled').exists():
    time.sleep(40)
    sys.exit(0)
if (root / 'failure').exists():
    print('final native diagnostic', file=sys.stderr, flush=True)
    sys.exit(19)
print(json.dumps({'type':'ready','protocol':'rpi'}), flush=True)
for line in sys.stdin:
    if json.loads(line).get('type') == 'stop': break
"#,
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        match mode {
            Mode::Failure => fs::write(root.join("failure"), "").unwrap(),
            Mode::Cancelled => fs::write(root.join("cancelled"), "").unwrap(),
            Mode::Ready => {}
        }
        Self { root }
    }

    fn options(&self) -> SessionOptions {
        SessionOptions {
            executable: self.root.join("native"),
            project: self.root.clone(),
            session_id: "stderr-fixture".into(),
            session_dir: Some(self.root.join("sessions")),
            saved_session: None,
            fork_source: None,
            timings_dir: self.root.join("timings"),
            model: None,
            provider: None,
            permission_gate: false,
            bypass: false,
            resources: None,
        }
    }

    fn daemon_alive(&self) -> bool {
        let Ok(pid) = fs::read_to_string(self.root.join("daemon.pid")) else {
            return false;
        };
        Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn stop_daemon(&self) {
        let path = self.root.join("daemon.pid");
        if let Ok(pid) = fs::read_to_string(&path) {
            let _ = Command::new("kill")
                .args(["-TERM", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            // Never signal a stale PID again during Drop.
            let _ = fs::remove_file(path);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_daemon();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_runtime_retires(mode: Mode) {
    let fixture = Fixture::new(mode);
    let options = fixture.options();
    let pid_path = fixture.root.join("daemon.pid");
    let (settled_tx, settled_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let owner = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            if matches!(mode, Mode::Cancelled) {
                let task = tokio::spawn(HostSession::spawn_options(options));
                let started = tokio::time::timeout(Duration::from_secs(5), async {
                    while !pid_path.exists() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await;
                task.abort();
                return match task.await {
                    Err(error) if error.is_cancelled() && started.is_ok() => Ok(()),
                    _ => Err("startup task did not reach fixture cancellation".into()),
                };
            }
            match HostSession::spawn_options(options).await {
                Ok(host) => {
                    host.shutdown().await;
                    if matches!(mode, Mode::Ready) {
                        Ok(())
                    } else {
                        Err("failed-start fixture unexpectedly sent ready".into())
                    }
                }
                Err(error) => {
                    if matches!(mode, Mode::Failure)
                        && error.contains("before ready (EOF)")
                        && error.contains("final native diagnostic")
                    {
                        Ok(())
                    } else {
                        Err(error)
                    }
                }
            }
        });
        let _ = settled_tx.send(result);
        // Regression: a blocking stderr read used to retain this runtime until
        // the escaped descendant closed fd 2, even after native exit/reaping.
        drop(runtime);
        let _ = dropped_tx.send(());
    });

    let settled = settled_rx.recv_timeout(Duration::from_secs(25));
    let retired = dropped_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let daemon_alive = fixture.daemon_alive();
    // Clean up even when the regression recurs, so a failing test cannot hang
    // Cargo or leak a detached daemon. Closing its stderr releases old readers.
    fixture.stop_daemon();
    if !retired {
        dropped_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("runtime must retire after fixture cleanup");
    }
    owner.join().unwrap();
    assert_eq!(settled, Ok(Ok(())), "host cleanup failed: {mode:?}");
    assert!(
        daemon_alive,
        "fixture must retain inherited stderr: {mode:?}"
    );
    assert!(retired, "inherited stderr retained the runtime: {mode:?}");
}

#[test]
fn shutdown_does_not_wait_for_escaped_descendant_stderr() {
    assert_runtime_retires(Mode::Ready);
}

#[test]
fn failed_startup_does_not_wait_for_escaped_descendant_stderr() {
    assert_runtime_retires(Mode::Failure);
}

#[test]
fn cancelled_startup_does_not_wait_for_escaped_descendant_stderr() {
    assert_runtime_retires(Mode::Cancelled);
}
