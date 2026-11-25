//! Configuration is supplied by the operator, never by tool arguments.
use rpi_extcore::shared_host::ConnectionConfig;
use std::path::Path;

#[cfg(unix)]
use rpi_extcore::shared_host::protocol::{AuthToken, HostId};
#[cfg(unix)]
use serde::Deserialize;
#[cfg(unix)]
use std::{path::PathBuf, time::Duration};

pub const CONFIG_ENV: &str = "RPI_SHARED_HOST_CONFIG";
#[cfg(unix)]
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[cfg(unix)]
#[derive(Deserialize)]
struct FileConfig {
    socket_path: PathBuf,
    host_id: HostId,
    token: AuthToken,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}

#[cfg(unix)]
fn default_timeout() -> u64 {
    15
}

pub fn from_environment() -> Result<ConnectionConfig, &'static str> {
    let path = std::env::var_os(CONFIG_ENV).ok_or("Set RPI_SHARED_HOST_CONFIG before loading rpi-host; tool arguments cannot choose credentials or endpoints")?;
    load(Path::new(&path))
}

#[cfg(unix)]
pub fn load(path: &Path) -> Result<ConnectionConfig, &'static str> {
    use std::{
        fs::OpenOptions,
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    if !path.is_absolute() {
        return Err("Host configuration path must be absolute");
    }
    // Check the opened FD, reject symlinks and avoid blocking on a FIFO. Parent
    // paths remain operator-trusted; this is not a hostile same-UID sandbox.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Cannot open private Host configuration")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect Host configuration")?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::Uid::effective().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_CONFIG_BYTES
    {
        return Err("Host configuration must be an owned private regular file with one link, at most 64 KiB");
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read Host configuration")?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("Host configuration exceeds 64 KiB");
    }
    // Additional fields allow using an existing host serve configuration. They
    // are not forwarded to the Host and cannot grant any permission here.
    let config: FileConfig =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid Host connection configuration")?;
    if !config.socket_path.is_absolute() || !(1..=300).contains(&config.timeout_seconds) {
        return Err("Host socket must be absolute and timeout_seconds must be 1..300");
    }
    Ok(ConnectionConfig {
        socket_path: config.socket_path,
        host_id: config.host_id,
        token: config.token,
        timeout: Duration::from_secs(config.timeout_seconds),
    })
}

#[cfg(not(unix))]
pub fn load(_: &Path) -> Result<ConnectionConfig, &'static str> {
    Err("Shared Host control requires Unix; no child-process fallback")
}
