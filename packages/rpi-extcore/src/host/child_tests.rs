#[cfg(windows)]
use super::*;
#[cfg(windows)]
#[tokio::test]
async fn rpc_eof_is_not_exit_and_reader_abort_cannot_drop_the_owned_reaper() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let root = std::env::temp_dir().join(format!("resource-child-exit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let lease = root.join("writer.lease");
    let script = r#"import os, sys, time, msvcrt
f=open(sys.argv[1], 'w+b', buffering=0)
f.write(b'x'); f.seek(0); msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
print('ready', flush=True)
os.close(1)
time.sleep(60)
"#;
    // The Windows venv launcher retains inherited stdout while waiting for its
    // real Python child. Use the actual interpreter so EOF is intentional.
    let probe = std::process::Command::new("python")
        .args(["-c", "import sys;print(sys._base_executable)"])
        .output()
        .unwrap();
    assert!(probe.status.success());
    let executable = String::from_utf8(probe.stdout).unwrap();
    let mut command = std::process::Command::new(executable.trim());
    command
        .args(["-c", script])
        .arg(&lease)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut tree = rpi_mcp_core::ProcessTree::spawn(&mut command).unwrap();
    let output =
        tokio::process::ChildStdout::from_std(tree.child_mut().stdout.take().unwrap()).unwrap();
    let owner = NativeChild::start(tree, None);
    let mut reader = BufReader::new(output);
    let mut ready = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.trim(), "ready");
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(tail.is_empty());
    assert!(
        std::fs::read(&lease).is_err(),
        "RPC EOF must not be mistaken for writer-lock release"
    );
    let reader = tokio::spawn(async move {
        drop(reader);
        futures_util::future::pending::<()>().await
    });
    reader.abort();
    let _ = reader.await;
    assert!(
        std::fs::read(&lease).is_err(),
        "aborting RPC owner must not infer OS exit"
    );
    owner.stop_checked().await.unwrap();
    assert_eq!(std::fs::read(&lease).unwrap(), b"x");
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn unconfirmed_exit_and_timed_out_waiter_cannot_release_launch_ownership() {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    let root =
        std::env::temp_dir().join(format!("native-unconfirmed-exit-{}", uuid::Uuid::new_v4()));
    let options = SessionOptions {
        executable: root.join("unused"),
        project: root.clone(),
        session_id: "test".into(),
        session_dir: None,
        saved_session: None,
        fork_source: None,
        timings_dir: root.join("timings"),
        model: None,
        provider: None,
        permission_gate: false,
        bypass: false,
        resources: None,
    };
    let guard = startup::LaunchGuard::acquire(&options).unwrap();
    let (exited, mut exit) = tokio::sync::watch::channel(None);
    let status = Arc::new(StdMutex::new(None));
    let confirmed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let evidence = confirmed.clone();
    let (allow_observation, observation) = std::sync::mpsc::channel();
    let mut first = true;
    let reaper = tokio::task::spawn_blocking(move || {
        NativeChild::confirm_exit(
            || {
                if first {
                    observation.recv_timeout(Duration::from_secs(5)).unwrap();
                    first = false;
                }
                if evidence.load(Ordering::Acquire) {
                    Ok(Some(std::process::ExitStatus::from_raw(0)))
                } else {
                    Err(std::io::Error::other("injected exit observation failure"))
                }
            },
            Some(guard),
            exited,
            status,
        )
    });
    // Explicitly advance: Tokio does not auto-advance paused time while the
    // blocking cleanup owner is still running.
    let waiter = tokio::spawn(NativeChild::wait_checked(exit.clone()));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(11)).await;
    let error = waiter.await.unwrap().unwrap_err();
    assert!(error.contains("deadline exceeded"), "{error}");
    assert!(startup::LaunchGuard::acquire(&options).is_err());
    allow_observation.send(()).unwrap();
    exit.wait_for(|state| *state == Some(false)).await.unwrap();
    assert!(NativeChild::wait_checked(exit.clone()).await.is_err());
    assert!(startup::LaunchGuard::acquire(&options).is_err());
    confirmed.store(true, Ordering::Release);
    reaper.await.unwrap();
    assert_eq!(*exit.borrow(), Some(true));
    drop(startup::LaunchGuard::acquire(&options).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
