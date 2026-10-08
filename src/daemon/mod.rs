//! Per-user daemon election and private Unix socket ownership.
pub mod client;
pub mod protocol;
pub mod registry;

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SocketPaths {
    pub run: PathBuf,
    pub lock: PathBuf,
    pub socket: PathBuf,
}

impl SocketPaths {
    pub fn new(data_dir: &Path) -> Self {
        let run = data_dir.join("run");
        Self {
            lock: run.join("daemon.lock"),
            socket: run.join("daemon.sock"),
            run,
        }
    }

    fn prepare(&self) -> io::Result<()> {
        // Existing system ancestors are not managed by Baleyg. Create each missing
        // component privately, even when the data directory itself is not present.
        let mut missing = Vec::new();
        let mut ancestor = self.run.as_path();
        while !ancestor.exists() {
            missing.push(ancestor.to_path_buf());
            ancestor = ancestor.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "daemon directory has no ancestor",
                )
            })?;
        }
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe daemon ancestor",
            ));
        }
        for dir in missing.iter().rev() {
            match fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
        }
        for dir in [self.run.parent().unwrap(), self.run.as_path()] {
            let metadata = fs::symlink_metadata(dir)?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe daemon directory",
                ));
            }
        }
        Ok(())
    }
}

pub struct SocketOwner {
    _lock: File,
    listener: UnixListener,
    socket: PathBuf,
    inode: u64,
}

impl SocketOwner {
    /// Only the elected lock holder is allowed to clear a crashed owner's socket.
    pub fn acquire(paths: &SocketPaths) -> io::Result<Option<Self>> {
        paths.prepare()?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&paths.lock)?;
        let metadata = lock.metadata()?;
        let named = fs::symlink_metadata(&paths.lock)?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || (metadata.dev(), metadata.ino()) != (named.dev(), named.ino())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe daemon lock",
            ));
        }
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error)
            };
        }
        match fs::symlink_metadata(&paths.socket) {
            Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(&paths.socket)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsafe daemon socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&paths.socket)?;
        fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
        let inode = fs::symlink_metadata(&paths.socket)?.ino();
        Ok(Some(Self {
            _lock: lock,
            listener,
            socket: paths.socket.clone(),
            inode,
        }))
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

impl Drop for SocketOwner {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.socket)
            .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.socket);
        }
    }
}

/// Drive idle release while a daemon owns the registry. The caller retains its
/// socket/listener guard and closes control connections when this returns.
/// Polling also observes work draining after an expired deadline without
/// resetting either clock. The command entry point wires this into its task.
pub async fn run_idle_lifecycle(
    registry: std::sync::Arc<tokio::sync::Mutex<registry::CheckoutRegistry>>,
) -> Result<(), registry::SelectionError> {
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(250));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        if registry
            .lock()
            .await
            .advance(std::time::Instant::now())?
            .exit
        {
            return Ok(());
        }
    }
}
