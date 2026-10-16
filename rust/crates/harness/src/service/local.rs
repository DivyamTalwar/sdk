use super::*;
use crate::{
    store::{HistoryReader, StreamAggregate},
    wire,
    wire_api::{self, HarnessWireApi},
    wire_codec::{decode_command_envelope, encode_authority, encode_event_envelope},
};
use acyclic_stream::{BoxProviderFuture, BoxProviderStream, StreamProvider as _};
use futures::StreamExt as _;

impl LocalHarness {
    /// The authenticated workspace owner and this local user's bearer grant.
    #[must_use]
    pub fn owner(&self) -> (&crate::core::Authority, &crate::core::Scope) {
        (self.verifier.audience(), &self.signed)
    }
    /// Durable owner and bearer scope for this local user's workspace.
    #[must_use]
    pub fn wire_owner(&self) -> (wire::Authority, wire::Scope) {
        (
            encode_authority(self.verifier.audience()),
            wire::Scope::from(&self.signed),
        )
    }

    /// Stable cursor at the beginning of this workspace's owner history.
    #[must_use]
    pub fn start_cursor(&self) -> wire::ReplayCursor {
        wire::ReplayCursor {
            authority: Some(encode_authority(self.verifier.audience())),
            generation: self.verifier.audience().id.clone(),
            revision: 0,
        }
    }
}

impl HarnessWireApi for LocalHarness {
    fn handshake<'a>(
        &'a self,
        request: wire::HandshakeRequest,
    ) -> BoxProviderFuture<'a, Result<wire::HandshakeResponse>> {
        Box::pin(async move { wire_api::negotiate(&request, &wire::CapabilitySet::default()) })
    }
    fn submit<'a>(
        &'a self,
        request: wire::CommandEnvelope,
    ) -> BoxProviderFuture<'a, Result<wire::Admission>> {
        Box::pin(async move {
            let operation = request.operation.clone();
            let (authority, command) = decode_command_envelope(request)?;
            self.verifier.verify_audience(&authority, &mut None)?;
            let content = crate::filesystem::FilesystemContentVerifier::new(
                self.files.clone(),
                &self.verifier,
                &self.signed,
                self.limits.file_bytes,
                None,
            )?;
            let mut aggregate = StreamAggregate::open(
                &self.stream,
                (
                    authority,
                    self.verifier.clone(),
                    crate::core::SchemaRegistry::new(),
                ),
                None,
                crate::store::default_projection_read_limits(),
                crate::store::DEFAULT_TERMINAL_EFFECTS,
                None,
                &mut None,
            )
            .await?
            .with_content_verifier(content)
            .with_merge_verifier(
                crate::filesystem::FilesystemProjectMergeVerifier::new(self.files.clone(), None)?,
            );
            let (state, error) = match aggregate.execute(command, None, &mut None).await {
                Ok(_) => (wire::AdmissionState::Accepted, None),
                Err(error @ Error::Indeterminate(_)) => (
                    wire::AdmissionState::Indeterminate,
                    Some(crate::encode_error(&error)),
                ),
                Err(error) => (
                    wire::AdmissionState::Rejected,
                    Some(crate::encode_error(&error)),
                ),
            };
            Ok(wire::Admission {
                operation,
                state: state as i32,
                error,
            })
        })
    }
    fn replay<'a>(
        &'a self,
        request: wire::ResumeRequest,
    ) -> BoxProviderFuture<'a, Result<BoxProviderStream<'static, Result<wire::Delivery>>>> {
        Box::pin(async move {
            wire_api::validate_resume_protocol(&request)?;
            if request.cursors.is_empty() {
                return Err(Error::Invalid("replay requires an explicit cursor".into()));
            }
            let mut streams = Vec::with_capacity(request.cursors.len());
            for cursor in request.cursors {
                if cursor.authority.as_ref() != self.start_cursor().authority.as_ref() {
                    return Err(Error::Unauthorized(
                        "replay authority belongs to another workspace".into(),
                    ));
                }
                if cursor.generation != self.verifier.audience().id {
                    return Err(Error::Conflict("local replay generation changed".into()));
                }
                let reader = HistoryReader::new(
                    &self.stream,
                    self.verifier.audience(),
                    &self.verifier,
                    &mut None,
                )?;
                // Pin validates the resume offset before opening a live, gapless producer.
                let pinned = reader.pin(cursor.revision, None).await?;
                let records = self
                    .stream
                    .follow(
                        acyclic_stream::StreamPath::new(
                            self.verifier.audience().stream_path(&mut None)?,
                        )
                        .map_err(storage)?,
                        cursor.revision,
                    )
                    .await
                    .map_err(storage)?;
                let verifier = self.verifier.clone();
                let stream = futures::stream::try_unfold(
                    (records, cursor, verifier, pinned.through_revision),
                    |(mut records, mut cursor, verifier, tail)| async move {
                        let Some(record) = records.next().await else {
                            return Ok(None);
                        };
                        let record = record.map_err(storage)?;
                        let (event, _) =
                            crate::store::history::verify_history_record_with_retained_quota(
                                &verifier,
                                verifier.audience(),
                                cursor.revision,
                                &record,
                                &mut None,
                            )?;
                        let delivery = wire::Delivery {
                            authority: cursor.authority.clone(),
                            generation: cursor.generation.clone(),
                            from_revision: cursor.revision,
                            through_revision: event.revision,
                            events: vec![encode_event_envelope(
                                verifier.audience(),
                                &event,
                                &mut None,
                            )?],
                            live: event.revision > tail,
                        };
                        cursor.revision = event.revision;
                        Ok(Some((delivery, (records, cursor, verifier, tail))))
                    },
                );
                streams
                    .push(Box::pin(stream) as BoxProviderStream<'static, Result<wire::Delivery>>);
            }
            Ok(Box::pin(futures::stream::select_all(streams))
                as BoxProviderStream<'static, Result<wire::Delivery>>)
        })
    }
    fn observe<'a>(
        &'a self,
        request: wire::ObserveRequest,
    ) -> BoxProviderFuture<'a, Result<wire::OperationStatus>> {
        Box::pin(async move { self.runtime.task_host().wire_observe(request).await })
    }
    fn cancel<'a>(
        &'a self,
        request: wire::CancelRequest,
    ) -> BoxProviderFuture<'a, Result<wire::CancelResponse>> {
        Box::pin(async move { self.runtime.task_host().wire_cancel(request).await })
    }
}
