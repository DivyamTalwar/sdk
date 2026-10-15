use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Owner {
    pub key: [u8; 32],
    pub agent: AgentId,
    pub task: TaskId,
    pub project: VolumeRef,
    workspace: Option<PathBuf>,
    pub(super) workspace_identity: Option<[u8; 16]>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

pub(super) fn directory(path: &Path) -> Result<PathBuf> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(storage)?;
    fs::canonicalize(path).map_err(storage)
}

impl Owner {
    pub(super) fn open(directory: &Path, selection: &LocalProject) -> Result<Self> {
        let workspace = match selection {
            LocalProject::Workspace(path) => Some(fs::canonicalize(path).map_err(storage)?),
            LocalProject::Volume(_) => None,
        };
        let path = directory.join("owner.json");
        let workspace_identity = workspace
            .as_ref()
            .map(|workspace| {
                let cancellation = acyclic_fs::CancellationToken::new();
                let work = acyclic_fs::NativeMaterializationWork::new(
                    acyclic_fs::WorkBudget::UNBOUNDED,
                    &cancellation,
                )
                .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
                Ok::<_, Error>(work.root_identity(workspace)?.to_bytes())
            })
            .transpose()?;
        let owner = match Self::load(&path) {
            Ok(owner) => owner,
            Err(Error::NotFound(_)) => {
                let project = match selection {
                    LocalProject::Volume(volume) => volume.clone(),
                    LocalProject::Workspace(_) => VolumeRef::new(
                        crate::resources::ProviderRef::new(
                            "local".into(),
                            "filesystem".into(),
                            "2".into(),
                            &mut None,
                        )?,
                        uuid::Uuid::new_v4().to_string(),
                        VolumeClass::Project,
                        VolumeOwner::Project(uuid::Uuid::new_v4().to_string()),
                        &mut None,
                    )?,
                };
                let mut key = [0; 32];
                key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                let owner = Self {
                    key,
                    agent: AgentId::new(),
                    task: TaskId::new(),
                    project,
                    workspace: workspace.clone(),
                    workspace_identity,
                };
                key.fill(0);
                if owner.publish(directory, &path)? {
                    owner
                } else {
                    Self::load(&path)?
                }
            }
            Err(error) => return Err(error),
        };
        owner.project.validate(&mut None)?;
        if owner.key == [0; 32]
            || owner.agent.into_bytes() == [0; 16]
            || owner.task.into_bytes() == [0; 16]
            || owner.project.class() != VolumeClass::Project
            || owner.project.provider().family() != "filesystem"
            || owner.workspace != workspace
            || owner.workspace_identity != workspace_identity
            || matches!(selection, LocalProject::Volume(volume) if volume != &owner.project)
        {
            return Err(Error::Unauthorized(
                "local state belongs to a different workspace or volume".into(),
            ));
        }
        Ok(owner)
    }

    fn load(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound("local owner".into())
            } else {
                storage(error)
            }
        })?;
        if !metadata.is_file() || metadata.len() > 65_536 {
            return Err(Error::Invalid("invalid local owner record".into()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
                return Err(Error::Unauthorized(
                    "local owner record has public permissions".into(),
                ));
            }
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(storage)?
            .take(65_537)
            .read_to_end(&mut bytes)
            .map_err(storage)?;
        let owner =
            serde_json::from_slice(&bytes).map_err(|error| Error::Invalid(error.to_string()));
        bytes.fill(0);
        owner
    }

    fn publish(&self, directory: &Path, path: &Path) -> Result<bool> {
        let temporary = directory.join(format!(".owner-{}.next", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary)?;
            let mut bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
            let written = file.write_all(&bytes);
            bytes.fill(0);
            written?;
            file.sync_all()?;
            drop(file);
            acyclic_native_runtime::durable_rename(
                &temporary,
                path,
                acyclic_native_runtime::RenameMode::NoReplace,
                None,
            )
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        match result {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(storage(error)),
        }
    }
}
