//! Safe platform lease primitive: noncreating open + nonblocking shared lock.
use std::{fs::File, io, path::Path};
#[cfg(all(test, any(unix, windows)))]
mod tests;
/// An observational fs2-compatible whole-file lease, not writer authorization.
/// Keep the owner through the protected decision. Drop closes its private,
/// synchronously-opened handle and releases the lock; no raw handle is exposed.
pub struct SharedFileLock {
    _file: File,
}
impl SharedFileLock {
    pub fn try_open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "shared lease requires a regular existing file",
            ));
        }
        try_lock_shared(&file)?;
        Ok(Self { _file: file })
    }
}
#[cfg(any(unix, windows))]
fn try_lock_shared(file: &File) -> io::Result<()> {
    fs2::FileExt::try_lock_shared(file).map_err(lock_error)
}

#[cfg(any(unix, windows))]
fn lock_error(error: io::Error) -> io::Error {
    // Windows reports a lock violation as a different error kind from flock.
    #[cfg(windows)]
    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
        return io::Error::new(io::ErrorKind::WouldBlock, error);
    }
    error
}

#[cfg(not(any(unix, windows)))]
fn try_lock_shared(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native observation lock unsupported on this platform",
    ))
}
