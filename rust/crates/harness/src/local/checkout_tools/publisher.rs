//! File publication shares the selected checkout and its command lock.
use super::*;
use crate::conversation::{
    BoundedContentFuture, ContentHold, ContentIo, ContentPublisher, FileRef,
};

type Publisher = FilesystemContentPublisher<LocalAuthorityBackend, LocalObjectBackend>;

pub(in crate::local) struct CheckoutPublisher {
    tools: Arc<CheckoutTools>,
    files: Arc<LocalFilesystem>,
    volume: VolumeRef,
    verifier: AuthorityVerifier,
    signed: Scope,
    maximum: u64,
}

enum Publication<'a> {
    Once {
        path: &'a str,
        media: &'a str,
        display: &'a str,
    },
    Path {
        path: &'a str,
        media: &'a str,
        display: &'a str,
    },
    Replace(&'a FileRef),
}

impl CheckoutPublisher {
    pub(in crate::local) fn new(
        tools: Arc<CheckoutTools>,
        files: Arc<LocalFilesystem>,
        volume: &VolumeRef,
        verifier: &AuthorityVerifier,
        signed: &Scope,
        maximum: u64,
    ) -> Self {
        Self {
            tools,
            files,
            volume: volume.clone(),
            verifier: verifier.clone(),
            signed: signed.clone(),
            maximum,
        }
    }

    fn writer(&self, view: &workspace::View) -> Result<Arc<Publisher>> {
        Publisher::new(
            self.files
                .for_workspace(&self.volume, view.workspace().clone())?,
            &self.volume,
            &self.verifier,
            &self.signed,
            self.maximum,
            None,
        )
    }

    async fn publish(
        &self,
        operation: OperationId,
        bytes: &[u8],
        publication: Publication<'_>,
    ) -> Result<FileRef> {
        serialized(&self.tools.state, async |mut state| {
            let State { active, watcher } = &mut *state;
            let view = active
                .as_ref()
                .unwrap_or_else(|| self.tools.workspace.view());
            self.tools.capture(view, watcher, operation, 0, PublicationPermit::Unrestricted).await?;
            let from = view
                .workspace()
                .head(
                    acyclic_fs::WorkBudget::UNBOUNDED,
                    &acyclic_fs::CancellationToken::new(),
                )
                .await
                .map_err(storage)?
                .value;
            let writer = self.writer(view)?;
            let file = match publication {
                Publication::Once {
                    path,
                    media,
                    display,
                } => {
                    writer
                        .stage_once(operation, path, bytes, media, display, None)?
                        .await?
                        .0
                }
                Publication::Path {
                    path,
                    media,
                    display,
                } => {
                    writer
                        .stage(operation, path, bytes, media, display, None)?
                        .await?
                        .0
                }
                Publication::Replace(source) => {
                    writer.stage_at(operation, source, bytes, None)?.await?.0
                }
            };
            let to = view
                .workspace()
                .head(
                    acyclic_fs::WorkBudget::UNBOUNDED,
                    &acyclic_fs::CancellationToken::new(),
                )
                .await
                .map_err(storage)?
                .value;
            let digest = blake3::derive_key(
                "acyclic.local.checkout-publication/v1",
                &operation.into_bytes(),
            );
            let mut identity = [0; 16];
            identity.copy_from_slice(&digest[..16]);
            crate::tools::git::publish_checkout_transition(
                &self.tools.workspace.path,
                self.tools.workspace.identity,
                &self.tools.workspace.store,
                acyclic_fs::OperationId::from_bytes(identity),
                from,
                to,
                &|| std::future::ready(Ok(())),
            )
            .await
            .map_err(storage)?;
            Ok(file)
        })
        .await
        .inspect_err(|error| tracing::error!(%operation, ?error, "checkout file publication did not complete"))
    }
}

impl ContentPublisher for CheckoutPublisher {
    fn volume(&self) -> &VolumeRef {
        &self.volume
    }

    fn supports_stage(&self, io: Option<&ContentIo>) -> Result<()> {
        if io.is_some() {
            return Err(Error::Unsupported(
                "checkout publication requires ordinary task execution".into(),
            ));
        }
        Ok(())
    }

    fn stage_once<'a>(
        &'a self,
        operation: OperationId,
        path: &'a str,
        bytes: &'a [u8],
        media: &'a str,
        display: &'a str,
        io: Option<&'a ContentIo>,
    ) -> Result<BoundedContentFuture<'a, (FileRef, Option<ContentHold<'a>>)>> {
        self.supports_stage(io)?;
        BoundedContentFuture::new(None, || async move {
            Ok((
                self.publish(
                    operation,
                    bytes,
                    Publication::Once {
                        path,
                        media,
                        display,
                    },
                )
                .await?,
                None,
            ))
        })
    }
    fn stage<'a>(
        &'a self,
        operation: OperationId,
        path: &'a str,
        bytes: &'a [u8],
        media: &'a str,
        display: &'a str,
        io: Option<&'a ContentIo>,
    ) -> Result<BoundedContentFuture<'a, (FileRef, Option<ContentHold<'a>>)>> {
        self.supports_stage(io)?;
        BoundedContentFuture::new(None, || async move {
            Ok((
                self.publish(
                    operation,
                    bytes,
                    Publication::Path {
                        path,
                        media,
                        display,
                    },
                )
                .await?,
                None,
            ))
        })
    }
    fn stage_at<'a>(
        &'a self,
        operation: OperationId,
        source: &'a FileRef,
        bytes: &'a [u8],
        io: Option<&'a ContentIo>,
    ) -> Result<BoundedContentFuture<'a, (FileRef, Option<ContentHold<'a>>)>> {
        self.supports_stage(io)?;
        BoundedContentFuture::new(None, || async move {
            Ok((
                self.publish(operation, bytes, Publication::Replace(source))
                    .await?,
                None,
            ))
        })
    }
    fn observe_staged_once<'a>(
        &'a self,
        operation: OperationId,
        maximum: u64,
        io: Option<&'a ContentIo>,
    ) -> Result<BoundedContentFuture<'a, (Option<FileRef>, Option<ContentHold<'a>>)>> {
        self.supports_stage(io)?;
        BoundedContentFuture::new(None, || async move {
            serialized(&self.tools.state, async |state| {
                let view = state
                    .active
                    .as_ref()
                    .unwrap_or_else(|| self.tools.workspace.view());
                Ok((
                    self.writer(view)?
                        .observe_staged_once(operation, maximum, None)?
                        .await?
                        .0,
                    None,
                ))
            })
            .await
        })
    }
}

#[cfg(all(test, not(acyclic_harness_loom)))]
#[tokio::test]
async fn file_publication_materializes_its_exact_committed_bytes() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let model = Model::new(
        "scripted".into(),
        "publication".into(),
        "1".into(),
        serde_json::Value::Null,
    )?;
    let provider = Arc::new(crate::test_support::ScriptedModelProvider::new([]));
    let harness =
        LocalHarness::open(LocalHarnessConfig::new(directory.path(), (model, provider))).await?;
    let tools = Arc::new(
        CheckoutTools::new(
            harness
                .workspace
                .as_ref()
                .ok_or_else(|| Error::NotFound("checkout".into()))?
                .clone(),
            &harness.project,
            Arc::new(acyclic_stream::SystemUnixMillisClock),
        )
        .await?,
    );
    let publisher = CheckoutPublisher::new(
        tools,
        harness.files.clone(),
        &harness.project,
        &harness.verifier,
        &harness.signed,
        harness.limits.file_bytes,
    );
    let (file, _) = publisher
        .stage_once(
            OperationId::new(),
            "published.txt",
            b"exact bytes",
            "text/plain",
            "published.txt",
            None,
        )?
        .await?;
    assert_eq!(file.volume(), &harness.project);
    assert_eq!(
        std::fs::read(directory.path().join("published.txt")).map_err(storage)?,
        b"exact bytes"
    );
    assert!(!directory.path().join(".system").exists());
    Ok(())
}
