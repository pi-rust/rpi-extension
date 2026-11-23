use super::*;

#[cfg(windows)]
#[path = "startup_windows_tests.rs"]
mod windows;

#[tokio::test]
async fn empty_ready_is_eof_not_a_json_parse_error() {
    let mut empty = &b""[..];
    let error = read_ready(&mut empty, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.contains("before ready (EOF)"), "{error}");
    assert!(!error.contains("invalid rpi ready frame"));

    let mut invalid = &b"not-json\n"[..];
    assert!(read_ready(&mut invalid, Duration::from_secs(1))
        .await
        .unwrap_err()
        .contains("invalid rpi ready frame"));
}

#[tokio::test]
async fn confirmed_exit_retires_a_reader_without_pipe_eof() {
    let (_writer, reader) = tokio::io::duplex(64);
    let (exited, exit) = tokio::sync::watch::channel(None);
    let capture = StderrCapture::start(reader, exit);
    exited.send_replace(Some(true));
    tokio::time::timeout(Duration::from_secs(2), capture.worker)
        .await
        .expect("inherited stderr must not retain the reader after confirmed exit")
        .unwrap();
    // `_writer` deliberately remains open throughout reader termination.
}

#[tokio::test]
async fn unconfirmed_exit_keeps_collecting_until_confirmation() {
    let (mut writer, reader) = tokio::io::duplex(64);
    let (exited, exit) = tokio::sync::watch::channel(Some(false));
    let mut capture = StderrCapture::start(reader, exit);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut capture.worker)
            .await
            .is_err()
    );
    writer.write_all(b"diagnostic after ready\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while capture.tail.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let tail: Vec<u8> = capture.tail.lock().unwrap().iter().copied().collect();
    assert_eq!(tail, b"diagnostic after ready\n");
    assert!(!capture.worker.is_finished());
    exited.send_replace(Some(true));
    tokio::time::timeout(Duration::from_secs(2), capture.worker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stderr_eof_does_not_publish_child_exit() {
    let (writer, reader) = tokio::io::duplex(64);
    let (_exited, exit) = tokio::sync::watch::channel(None);
    let observer = exit.clone();
    let capture = StderrCapture::start(reader, exit);
    drop(writer);
    tokio::time::timeout(Duration::from_secs(2), capture.worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*observer.borrow(), None, "pipe EOF is not OS exit evidence");
}

#[tokio::test]
async fn cancelling_a_pending_read_closes_the_pipe() {
    let (mut writer, reader) = tokio::io::duplex(64);
    let (_exited, exit) = tokio::sync::watch::channel(None);
    let capture = StderrCapture::start(reader, exit);
    tokio::task::yield_now().await;
    capture.worker.abort();
    assert!(capture.worker.await.unwrap_err().is_cancelled());
    assert!(writer.write_all(b"no reader").await.is_err());
}

#[cfg(windows)]
#[test]
fn windows_pending_pipe_read_does_not_retain_runtime() {
    let (writer_tx, writer_rx) = std::sync::mpsc::channel();
    let (retired_tx, retired_rx) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (writer, _exited, _capture) = runtime.block_on(async {
            let (writer, reader) = windows_stderr_pipe().await.unwrap();
            let (exited, exit) = tokio::sync::watch::channel(None);
            let capture = StderrCapture::start(reader, exit);
            // Start an actual Windows pipe read, not a duplex mock. The writer
            // and exit sender remain live while runtime cancellation runs.
            tokio::task::yield_now().await;
            assert!(!capture.worker.is_finished());
            (writer, exited, capture)
        });
        writer_tx.send(writer).unwrap();
        drop(runtime);
        retired_tx.send(()).unwrap();
    });
    let writer = writer_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let retired = retired_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    // Release the test's pipe even on regression before joining the owner.
    drop(writer);
    if !retired {
        retired_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    owner.join().unwrap();
    assert!(retired, "Windows stderr read retained the Tokio runtime");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::service::{open_session, BackendConfig, Service};
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new(script: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("native-startup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let executable = root.join("native");
            std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self { root }
        }
        fn options(&self) -> SessionOptions {
            SessionOptions {
                executable: self.root.join("native"),
                project: self.root.clone(),
                session_id: "startup-test".into(),
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
        async fn service(&self) -> Arc<Service> {
            let service = Service::new(BackendConfig {
                project: self.root.clone(),
                rpi_bin: self.root.join("native"),
                registry: self.root.join("registry.json"),
                session_dir: Some(self.root.join("sessions")),
                ..BackendConfig::default()
            })
            .unwrap();
            // The fixture registers metadata only. Native owns transcript I/O.
            let mut registry =
                crate::registry::Registry::load(service.config.registry.clone()).unwrap();
            registry
                .upsert(crate::registry::RegistryEntry {
                    session_id: "startup-test".into(),
                    project_id: None,
                    title: None,
                    parent_session_id: None,
                    fork_entry_id: None,
                    relation: None,
                    created_at: 1,
                    updated_at: 1,
                    last_turn_ended_at: None,
                })
                .unwrap();
            // Reload to exercise the same public Service path as real adapters.
            Service::new(service.config.clone()).unwrap()
        }
        async fn wait_started(&self) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !self.root.join("started").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const READY_SCRIPT: &str = r#"
printf 'started\n' >> started
printf '{"type":"ready","protocol":"rpi"}\n'
while IFS= read -r line; do
    case "$line" in *'"type":"stop"'*) exit 0;; esac
done
"#;

    #[tokio::test]
    async fn early_exit_reports_native_stderr_and_releases_launch_ownership() {
        let f = Fixture::new("printf 'writer lease is already held\\n' >&2\nexit 23");
        for _ in 0..2 {
            let error = HostSession::spawn_options(f.options())
                .await
                .err()
                .expect("startup must fail");
            assert!(error.contains("before ready (EOF)"), "{error}");
            assert!(error.contains("writer lease is already held"), "{error}");
            assert!(error.contains("pid ") && error.contains("exit"), "{error}");
            assert!(!error.contains("invalid rpi ready frame"), "{error}");
            assert!(!error.contains("cleanup is still in progress"), "{error}");
        }
    }

    #[tokio::test]
    async fn stderr_is_drained_but_only_a_bounded_tail_is_returned() {
        let f = Fixture::new(
            r#"
printf 'discarded-head\n' >&2
i=0
while [ "$i" -lt 400 ]; do
    printf 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\n' >&2
    i=$((i+1))
done
printf 'final native diagnostic\n' >&2
exit 1
"#,
        );
        let error = HostSession::spawn_options(f.options())
            .await
            .err()
            .expect("startup must fail");
        assert!(error.contains("final native diagnostic"));
        assert!(!error.contains("discarded-head"));
        assert!(
            error.len() < STDERR_LIMIT + 1024,
            "diagnostic was not bounded"
        );
    }

    #[tokio::test]
    async fn live_child_owns_launch_lock_until_confirmed_shutdown() {
        let f = Fixture::new(READY_SCRIPT);
        let host = HostSession::spawn_options(f.options()).await.unwrap();
        let error = HostSession::spawn_options(f.options())
            .await
            .err()
            .expect("duplicate must fail");
        assert!(error.contains("already owned"), "{error}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("started"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        host.shutdown().await;
        let second = HostSession::spawn_options(f.options()).await.unwrap();
        second.shutdown().await;
        assert_eq!(
            std::fs::read_to_string(f.root.join("started"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn fork_cut_entrypoint_preserves_launch_ownership_and_arguments() {
        let f = Fixture::new(&format!("printf '%s\\n' \"$@\" > argv\n{READY_SCRIPT}"));
        let mut options = f.options();
        options.fork_source = Some("source-session".into());
        let host =
            HostSession::spawn_options_with_fork_cut(options.clone(), Some("assistant-entry"))
                .await
                .unwrap();
        let args = std::fs::read_to_string(f.root.join("argv")).unwrap();
        assert!(args.contains("--fork\nsource-session\n"), "{args}");
        assert!(args.contains("--fork-entry\nassistant-entry\n"), "{args}");
        assert!(args.contains("--fork-position\nat\n"), "{args}");
        let error = HostSession::spawn_options(options.clone())
            .await
            .err()
            .expect("legacy entrypoint must respect the same launch guard");
        assert!(error.contains("already owned"), "{error}");
        host.shutdown().await;
        let reopened = HostSession::spawn_options_with_fork_cut(options, Some("assistant-entry"))
            .await
            .unwrap();
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_ready_wait_retains_reaper_until_launch_lock_is_released() {
        let f = Fixture::new("printf 'started\\n' > started\nexec sleep 60");
        let options = f.options();
        let task = tokio::spawn(HostSession::spawn_options(options.clone()));
        f.wait_started().await;
        assert!(LaunchGuard::acquire(&options).is_err());
        task.abort();
        assert!(task
            .await
            .err()
            .expect("cancelled startup task")
            .is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if LaunchGuard::acquire(&options).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled startup must finish owned cleanup");
    }

    #[tokio::test]
    async fn concurrent_service_opens_publish_only_one_native_writer() {
        let f = Fixture::new(READY_SCRIPT);
        let service = f.service().await;
        let (a, b) = tokio::join!(
            open_session(&service, "startup-test".into()),
            open_session(&service, "startup-test".into())
        );
        assert_eq!(a.1["status"], "ready", "{a:?}");
        assert_eq!(b.1["status"], "ready", "{b:?}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("started"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn separate_services_cannot_duplicate_a_launch_in_the_same_registry() {
        let f = Fixture::new(READY_SCRIPT);
        let service = f.service().await;
        let other = Service::new(service.config.clone()).unwrap();
        assert_eq!(
            open_session(&service, "startup-test".into()).await.1["status"],
            "ready"
        );
        let rejected = open_session(&other, "startup-test".into()).await;
        assert_eq!(rejected.1["code"], "host_start_failed");
        assert!(rejected.1["error"]
            .as_str()
            .unwrap()
            .contains("already owned"));
        assert_eq!(
            std::fs::read_to_string(f.root.join("started"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        service.shutdown().await;
        assert_eq!(
            open_session(&other, "startup-test".into()).await.1["status"],
            "ready"
        );
        other.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_open_request_keeps_its_service_owner_and_deduplicates_next_open() {
        let f = Fixture::new(
            r#"
printf 'started\n' >> started
while [ ! -e release ]; do sleep 0.02; done
printf '{"type":"ready","protocol":"rpi"}\n'
while IFS= read -r line; do
    case "$line" in *'"type":"stop"'*) exit 0;; esac
done
"#,
        );
        let service = f.service().await;
        let owner = service.clone();
        let task = tokio::spawn(async move { open_session(&owner, "startup-test".into()).await });
        f.wait_started().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        std::fs::write(f.root.join("release"), "").unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            open_session(&service, "startup-test".into()),
        )
        .await
        .unwrap();
        assert_eq!(result.1["status"], "ready", "{result:?}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("started"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        service.shutdown().await;
    }
}
