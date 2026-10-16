use super::*;

impl PreparationRecord for crate::context::ContextSummary {}

impl<P, A, O> FilesystemExecutionJournal<P, A, O>
where
    P: StreamProvider,
    A: AsyncAuthorityStore + 'static,
    O: AsyncObjectStore + 'static,
{
    pub(super) async fn authenticate_summary_with_io(
        &self,
        file: &FileRef,
        io: Option<&PreparationIo>,
    ) -> Result<bool> {
        let before = io.map(PreparationIo::owned_bytes).transpose()?.unwrap_or(0);
        let result = async {
            if file.volume() != &self.volume || file.descriptor().media_type() != "text/plain" {
                return Ok(false);
            }
            if let Some(io) = io {
                io.charge(acyclic_fs::WorkCounters {
                    items_examined: file.path().len() as u64,
                    ..Default::default()
                })?;
            }
            let Some(operation) = crate::executor::summary_identity::summary_operation(file.path())
            else {
                return Ok(false);
            };
            crate::executor::with_history_io(io, |work| {
                ContentGrant::authorize_volume(
                    &self.verifier,
                    &self.scope,
                    &self.volume,
                    VolumeOperation::Read,
                    self.maximum_payload_bytes,
                    work,
                )
            })?;
            let key = crate::executor::with_history_io(io, |work| file.read_capability(work))?;
            let key_hold = io
                .map(|io| io.guard_owned(key.capacity() as u64))
                .transpose()?;
            let (retry, path) = self.stage_identity_with_io(operation, &key, io)?;
            let path_hold = io
                .map(|io| io.guard_owned((retry.as_str().len() + path.capacity()) as u64))
                .transpose()?;
            let workspace = super::super::fork_preparer_io::content_workspace(
                &self.host.provider,
                &self.volume,
                io,
            )?;
            let absolute = crate::executor::with_history_io(io, |work| {
                crate::executor::preparation_format(format_args!("/{path}"), work)
            })?;
            let absolute_hold = io
                .map(|io| io.guard_owned(absolute.capacity() as u64))
                .transpose()?;
            if let Some((owner, _)) = &self.owner {
                owner
                    .verify_read_access(
                        &mut io
                            .map(PreparationIo::history_io)
                            .as_mut()
                            .map(|work| work as &mut dyn crate::store::HistoryIo),
                    )
                    .await?;
            }
            // This protected provenance is written only by publish_summary. Query
            // it directly, once; do not grant a directory or rescan model history.
            let bytes = match self
                .host
                .read(&workspace, None, &absolute, self.maximum_payload_bytes, io)
                .await
            {
                Ok(bytes) => bytes,
                Err(error) if super::super::fork_preparer::journal_absent(&error) => {
                    return Ok(false);
                }
                Err(error) => return Err(error),
            };
            let allowed = match io {
                Some(io) => {
                    let (summary, hold) = io.decode(
                        std::marker::PhantomData::<crate::context::ContextSummary>,
                        &bytes,
                        crate::wire::ErrorCode::Invalid,
                    )?;
                    let allowed = summary.operation_id == operation
                        && summary.source_messages > 0
                        && summary.output == *file;
                    drop(summary);
                    drop(hold);
                    allowed
                }
                None => {
                    let summary: crate::context::ContextSummary =
                        crate::contract::json_from_slice(&bytes)
                            .map_err(|error| Error::Storage(error.to_string()))?;
                    summary.operation_id == operation
                        && summary.source_messages > 0
                        && summary.output == *file
                }
            };
            drop(bytes);
            if let Some((owner, _)) = &self.owner {
                owner
                    .verify_read_access(
                        &mut io
                            .map(PreparationIo::history_io)
                            .as_mut()
                            .map(|work| work as &mut dyn crate::store::HistoryIo),
                    )
                    .await?;
            }
            drop(absolute);
            drop(absolute_hold);
            drop(retry);
            drop(path);
            drop(path_hold);
            drop(key);
            drop(key_hold);
            Ok(allowed)
        }
        .await;
        PreparationIo::release_since(io, Some(before))?;
        result
    }
}
