//! Client for the native local Host v1 opening handshake and JSONL RPC stream.
use super::*;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};
use tokio::net::{unix::OwnedReadHalf, unix::OwnedWriteHalf, UnixStream};

pub(crate) async fn connect(path: &Path) -> Result<UnixStream, String> {
    let parent = path
        .parent()
        .ok_or("local Host socket has no parent directory")?;
    let uid = nix::unistd::geteuid().as_raw();
    let directory = std::fs::symlink_metadata(parent)
        .map_err(|error| format!("local Host socket directory is unavailable: {error}"))?;
    if !path.is_absolute()
        || !directory.is_dir()
        || directory.uid() != uid
        || directory.mode() & 0o077 != 0
    {
        return Err(
            "local Host socket must be in a private directory owned by the Web user".into(),
        );
    }
    let socket = std::fs::symlink_metadata(path)
        .map_err(|error| format!("local Host socket is unavailable: {error}"))?;
    if !socket.file_type().is_socket() || socket.uid() != uid {
        return Err("local Host socket must be owned by the Web user".into());
    }
    let stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(path))
        .await
        .map_err(|_| "local Host connection timed out")?
        .map_err(|error| format!("could not connect to local Host: {error}"))?;
    if stream.peer_cred().map_err(|error| error.to_string())?.uid() != uid {
        return Err("local Host peer belongs to another user".into());
    }
    Ok(stream)
}

pub(super) async fn open(
    path: &Path,
    options: &SessionOptions,
    fork_cut: Option<&str>,
    resource_file: Option<&resources::StartupFile>,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Value), String> {
    let stream = connect(path).await?;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let resource_path = |flag| {
        resource_file
            .filter(|file| file.flag == flag)
            .map(|file| &file.path)
    };
    let forking = options.saved_session.is_none() && options.fork_source.is_some();
    let mut request = serde_json::to_vec(&json!({
        "version": 1,
        "workspace": options.project,
        "rpc": {
            "provider": options.provider,
            "model": options.model,
            "session": options.saved_session,
            "session_id": (options.saved_session.is_none() && !forking).then_some(&options.session_id),
            "session_dir": options.session_dir,
            "fork": options.fork_source.as_ref().filter(|_| forking),
            "fork_entry": fork_cut.filter(|_| forking),
            "fork_at": fork_cut.filter(|_| forking).map(|_| true),
            "resource_config": resource_path("--resource-config"),
            "resource_transition": resource_path("--resource-transition"),
            "trust_override": null,
            "no_session": false,
            "native_durable": false,
            "snapshots": false,
            "permission_gate": options.permission_gate && !options.bypass,
            "bypass": options.bypass,
        }
    }))
    .map_err(|error| error.to_string())?;
    request.push(b'\n');
    tokio::time::timeout(Duration::from_secs(5), write.write_all(&request))
        .await
        .map_err(|_| "local Host opening request timed out")?
        .map_err(|error| error.to_string())?;
    let started = tokio::time::Instant::now();
    let opened = read_startup_frame(
        &mut read,
        started + Duration::from_secs(90),
        "local Host session opening timed out",
    )
    .await?;
    if opened["type"] == "error" {
        return Err(format!(
            "local Host rejected session: {}",
            opened["message"]
                .as_str()
                .unwrap_or("missing error message")
        ));
    }
    let session_id = opened["session_id"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok());
    if opened["type"] != "opened"
        || opened["agent_id"]
            .as_str()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .is_none()
        || !matches!(opened["host_pid"].as_u64(), Some(pid) if pid > 0 && pid <= u64::from(u32::MAX))
        || session_id.is_none()
        || (!forking && session_id != uuid::Uuid::parse_str(&options.session_id).ok())
    {
        return Err("local Host opening reply did not identify the requested session".into());
    }
    let ready = read_startup_frame(
        &mut read,
        started + Duration::from_secs(120),
        "local Host RPC readiness timed out",
    )
    .await?;
    if ready["type"] != "ready" || ready["protocol"] != "rpi" {
        return Err("local Host did not advertise protocol rpi".into());
    }
    Ok((read, write, ready))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn dropped_socket_session_holds_launch_guard_until_native_eof() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("rpc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let options = SessionOptions {
            executable: "never-spawned".into(),
            project: root.path().into(),
            session_id: uuid::Uuid::new_v4().to_string(),
            session_dir: Some(root.path().join("history")),
            saved_session: None,
            fork_source: None,
            timings_dir: root.path().join("timings"),
            model: None,
            provider: None,
            permission_gate: false,
            bypass: false,
            resources: None,
        };
        let id = options.session_id.clone();
        let (write_closed, writer_closed) = oneshot::channel();
        let (retire, retired) = oneshot::channel();
        let host = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            let mut frame = String::new();
            socket.read_line(&mut frame).await.unwrap();
            let request: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(request["version"], 1);
            assert_eq!(request["rpc"]["session_id"], id);
            let opened = json!({"type":"opened", "host_pid":std::process::id(),
                "agent_id":uuid::Uuid::new_v4().to_string(), "session_id":id});
            socket
                .get_mut()
                .write_all(
                    format!("{opened}\n{{\"type\":\"ready\",\"protocol\":\"rpi\"}}\n").as_bytes(),
                )
                .await
                .unwrap();
            frame.clear();
            assert_eq!(socket.read_line(&mut frame).await.unwrap(), 0);
            write_closed.send(()).unwrap();
            retired.await.unwrap();
        });
        let session = HostSession::connect_local_socket(options.clone(), None, &path)
            .await
            .unwrap();
        let worker = session.worker.clone();
        drop(session);
        writer_closed.await.unwrap();
        assert!(startup::LaunchGuard::acquire(&options).is_err());
        retire.send(()).unwrap();
        host.await.unwrap();
        worker.lock().await.take().unwrap().await.unwrap();
        assert!(startup::LaunchGuard::acquire(&options).is_ok());
    }
}
