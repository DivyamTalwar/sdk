use super::*;
use acyclic_fs::{
    CancellationToken, MountOptions, MountPublication, WorkBudget, kernel::NamespacePath,
};

pub(super) const SCRATCH_PATH: &str = ".volumes/scratch";

pub(in crate::local) fn excluded(
    root: &Path,
    config: acyclic_fs::VolumeConfig,
) -> Result<Vec<NamespacePath>> {
    let destination = root.join(SCRATCH_PATH);
    std::iter::once(destination.clone())
        .chain(acyclic_fs::native_mount_control_paths(&destination).map_err(Error::from)?)
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| Error::Invalid("mount metadata lies outside the task view".into()))?;
            let relative = relative
                .to_str()
                .ok_or_else(|| Error::Invalid("mount path is not UTF-8".into()))?;
            let path = acyclic_fs::path::PortablePath::parse(
                &format!("/{}", relative.replace('\\', "/")),
                config.limits,
            )
            .map_err(storage)?;
            NamespacePath::from_portable_in_profile(&path, config.profile, config.limits)
                .map_err(storage)
        })
        .collect()
}

impl LocalWorkspace {
    pub(in crate::local) async fn with_scratch(
        mut self,
        project: &VolumeRef,
        scratch: &VolumeRef,
    ) -> Result<Self> {
        let cancellation = CancellationToken::new();
        let mut mounts = mounts::owner(
            &self.path,
            project,
            None,
            [("scratch".into(), scratch.clone(), None)],
        )?;
        for (relative, mount) in mounts.mounts_mut() {
            if relative.is_empty() {
                continue;
            }
            let source = self
                .filesystem
                .open_workspace(
                    &mount.view.volume.storage_name(&mut None)?,
                    WorkBudget::UNBOUNDED,
                    &cancellation,
                )
                .await
                .map_err(|failure| Error::from(failure.error))?
                .value;
            let destination = self.path.join(relative);
            create_mount_directory(&self.path, Path::new(relative))?;
            let options = if mount.mode.read_only() {
                MountOptions::read_only()
            } else {
                MountOptions::read_write().publication(MountPublication::Manual)
            };
            mount.view.source = Some(Arc::new(
                source
                    .mount(&destination, options)
                    .await
                    .map_err(Error::from)?,
            ));
        }
        self.mounts = Arc::new(mounts);
        Ok(self)
    }
}

fn create_mount_directory(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        let std::path::Component::Normal(name) = part else {
            return Err(Error::Invalid("invalid mount path".into()));
        };
        path.push(name);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let entry = std::fs::symlink_metadata(&path).map_err(mount_io)?;
                if !entry.is_dir() || entry.file_type().is_symlink() {
                    return Err(Error::Invalid(
                        "mount parent is not an ordinary directory".into(),
                    ));
                }
            }
            Err(error) => return Err(mount_io(error)),
        }
    }
    Ok(())
}

fn mount_io(cause: std::io::Error) -> Error {
    acyclic_fs::NativeTreeMaterializationError::Io(cause).into()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn mount_directory_preserves_the_native_io_cause() -> Result<()> {
        let directory = tempfile::tempdir().map_err(storage)?;
        let error =
            create_mount_directory(&directory.path().join("missing"), Path::new(SCRATCH_PATH))
                .expect_err("the mount parent is absent");
        assert!(matches!(error, Error::NativeWork {
            cause: acyclic_fs::NativeTreeMaterializationError::Io(error), ..
        } if error.kind() == std::io::ErrorKind::NotFound));
        Ok(())
    }

    #[test]
    fn mount_parent_cannot_redirect_creation_outside_the_view() -> Result<()> {
        let root = tempfile::tempdir().map_err(storage)?;
        let outside = tempfile::tempdir().map_err(storage)?;
        std::os::unix::fs::symlink(outside.path(), root.path().join(".volumes"))
            .map_err(storage)?;
        assert!(matches!(
            create_mount_directory(root.path(), Path::new(SCRATCH_PATH)),
            Err(Error::Invalid(_))
        ));
        assert!(!outside.path().join("scratch").exists());
        Ok(())
    }
}
