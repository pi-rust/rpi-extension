//! Windows end-to-end coverage uses a trusted batch fixture, not installed
//! native rpi. It exercises the real HostSession -> ProcessTree -> inherited
//! named-pipe writer -> startup diagnostic / shutdown path.
use super::*;

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(failure: bool) -> Self {
        let root = std::env::temp_dir().join(format!("native-stderr-win-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("native.cmd"),
            concat!(
                "@echo off\r\n",
                "echo windows before-ready diagnostic 1>&2\r\n",
                "if exist \"%~dp0failure\" exit /b 19\r\n",
                "echo {\"type\":\"ready\",\"protocol\":\"rpi\"}\r\n",
                "set /p ignored=\r\n",
                "echo windows after-ready diagnostic 1>&2\r\n",
                "exit /b 0\r\n",
            ),
        )
        .unwrap();
        if failure {
            std::fs::write(root.join("failure"), "").unwrap();
        }
        Self { root }
    }

    fn options(&self) -> SessionOptions {
        SessionOptions {
            executable: self.root.join("native.cmd"),
            project: self.root.clone(),
            session_id: "windows-stderr-fixture".into(),
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn child_stderr_reaches_startup_failure_and_cleanup_allows_retry() {
    let fixture = Fixture::new(true);
    for _ in 0..2 {
        let error = HostSession::spawn_options(fixture.options())
            .await
            .err()
            .expect("fixture must exit before ready");
        assert!(error.contains("before ready (EOF)"), "{error}");
        assert!(error.contains("windows before-ready diagnostic"), "{error}");
        assert!(error.contains("pid ") && error.contains("exit"), "{error}");
        assert!(!error.contains("already owned"), "{error}");
    }
}

#[tokio::test]
async fn child_with_inherited_stderr_can_start_shutdown_and_reopen() {
    let fixture = Fixture::new(false);
    let first = HostSession::spawn_options(fixture.options()).await.unwrap();
    let duplicate = HostSession::spawn_options(fixture.options())
        .await
        .err()
        .expect("live duplicate must fail");
    assert!(duplicate.contains("already owned"), "{duplicate}");
    first.shutdown().await;
    let second = HostSession::spawn_options(fixture.options()).await.unwrap();
    second.shutdown().await;
}
