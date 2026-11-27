use super::*;
#[test]
fn observation_lease_is_noncreating_shared_and_excludes_independent_writer_handles() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "shared-native-lease-{}-{nonce}.lock",
        std::process::id()
    ));
    assert!(matches!(SharedFileLock::try_open(&path),Err(e) if e.kind()==io::ErrorKind::NotFound));
    assert!(!path.exists());
    std::fs::write(&path, b"native owned lock: never truncate").unwrap();
    let first = SharedFileLock::try_open(&path).unwrap();
    let second = SharedFileLock::try_open(&path).unwrap();
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let try_write = || fs2::FileExt::try_lock_exclusive(&writer).map_err(lock_error);
    assert_eq!(try_write().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(first);
    assert_eq!(try_write().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(second);
    try_write().unwrap();
    assert!(
        matches!(SharedFileLock::try_open(&path),Err(e) if e.kind()==io::ErrorKind::WouldBlock)
    );
    drop(writer);
    let last = SharedFileLock::try_open(&path).unwrap();
    drop(last);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"native owned lock: never truncate"
    );
    std::fs::remove_file(path).unwrap();
}
