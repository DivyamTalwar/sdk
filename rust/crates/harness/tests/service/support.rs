use super::harness::{
    Error, LocalHarness, LocalHarnessConfig, Result,
    conversation::{VolumeClass, VolumeOwner, VolumeRef},
    filesystem::FilesystemHost,
    model::{Model, ModelProvider},
    resources::ProviderRef,
    test_support::{ScriptedModelProvider, ScriptedTurn},
};
use std::{path::Path, sync::Arc};
fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

pub(super) fn open(
    directory: &Path,
) -> acyclic_stream::BoxProviderFuture<'static, Result<Arc<LocalHarness>>> {
    open_with_provider(
        directory,
        Arc::new(ScriptedModelProvider::new([ScriptedTurn::Text(
            "done".into(),
        )])),
    )
}

pub(super) fn open_with_provider(
    directory: &Path,
    model_provider: Arc<dyn ModelProvider>,
) -> acyclic_stream::BoxProviderFuture<'static, Result<Arc<LocalHarness>>> {
    let directory = directory.to_path_buf();
    Box::pin(async move {
        acyclic_native_runtime::initialize_native_io_services().map_err(storage)?;
        let provider = ProviderRef::new("test".into(), "filesystem".into(), "2".into(), &mut None)?;
        let volume = VolumeRef::new(
            provider.clone(),
            "project".into(),
            VolumeClass::Project,
            VolumeOwner::Project("test".into()),
            &mut None,
        )?;
        let directory = directory.join("filesystem");
        let fs = acyclic_fs::Fs::local(acyclic_fs::LocalOptions::new(&directory))
            .await
            .map_err(storage)?;
        FilesystemHost::new(fs, &provider, None)?
            .create_volume(&volume)
            .await?;
        let model = Model::new(
            "scripted".into(),
            "test".into(),
            "1".into(),
            serde_json::Value::Null,
        )?;
        Ok(Arc::new(
            LocalHarness::open(
                LocalHarnessConfig::new(directory, (model, model_provider)).volume(volume),
            )
            .await?,
        ))
    })
}
