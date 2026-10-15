use super::{frames, storage};
use crate::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

/// One local endpoint and its persistent process lock in a user-owned directory.
#[derive(Clone, Debug)]
pub struct ServiceEndpoint {
    pub(super) directory: PathBuf,
    #[cfg(windows)]
    pub(super) pipe: String,
    #[cfg(windows)]
    pub(super) security: std::sync::Arc<acyclic_native_runtime::UserPipeSecurity>,
}

impl ServiceEndpoint {
    /// Selects the single service shared by this user's workspaces.
    pub fn per_user() -> Result<Self> {
        let root = dirs::data_local_dir()
            .ok_or_else(|| Error::Unsupported("per-user data directory unavailable".into()))?;
        Self::in_directory(root.join("acyclic/harness/service"))
    }

    /// Selects an isolated endpoint, including for in-process socket tests.
    pub fn in_directory(directory: impl AsRef<Path>) -> Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(directory.as_ref()).map_err(storage)?;
        let directory = std::fs::canonicalize(directory).map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if std::fs::metadata(&directory)
                .map_err(storage)?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err(Error::Unauthorized(
                    "service directory is accessible to other users".into(),
                ));
            }
        }
        Ok(Self {
            #[cfg(windows)]
            security: std::sync::Arc::new(
                acyclic_native_runtime::UserPipeSecurity::current_user().map_err(storage)?,
            ),
            #[cfg(windows)]
            pipe: format!(
                r"\\.\pipe\acyclic-harness-{}",
                blake3::hash(directory.as_os_str().as_encoded_bytes()).to_hex()
            ),
            directory,
        })
    }

    pub(super) fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("instance.lock"))
            .map_err(storage)?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                Error::Conflict("harness service is already running".into())
            } else {
                storage(error)
            }
        })?;
        Ok(file)
    }

    pub(super) async fn connect(&self) -> Result<frames::Connection> {
        #[cfg(unix)]
        let socket = tokio::net::UnixStream::connect(self.directory.join("harness.sock"))
            .await
            .map_err(storage)?;
        #[cfg(windows)]
        let socket = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match tokio::net::windows::named_pipe::ClientOptions::new().open(&self.pipe) {
                    Ok(socket) => break socket,
                    Err(error) if error.raw_os_error() == Some(231) => {
                        // Another client took the available instance. Wait for the next
                        // listener in the kernel; availability may race again at open.
                        let remaining =
                            deadline.saturating_duration_since(std::time::Instant::now());
                        if remaining.is_zero() {
                            return Err(Error::Storage("local pipe connection timed out".into()));
                        }
                        let pipe = self.pipe.clone();
                        let millis = u32::try_from(remaining.as_millis())
                            .map_err(storage)?
                            .max(1);
                        tokio::task::spawn_blocking(move || {
                            acyclic_native_runtime::wait_named_pipe(
                                std::ffi::OsStr::new(&pipe),
                                millis,
                            )
                        })
                        .await
                        .map_err(storage)?
                        .map_err(storage)?;
                    }
                    Err(error) => return Err(storage(error)),
                }
            }
        };
        #[cfg(windows)]
        self.security.authenticate(&socket).map_err(storage)?;
        Ok(frames::connection(socket))
    }
}
