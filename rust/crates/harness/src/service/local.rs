use super::*;
use crate::{
    store::StreamAggregate,
    wire,
    wire_api::{self, HarnessWireApi},
    wire_codec::{decode_command_envelope, encode_authority},
};
use acyclic_stream::{BoxProviderFuture, BoxProviderStream};

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
            let provider = self.runtime.task_host().workflow_stream();
            let mut aggregate = StreamAggregate::open(
                &provider,
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
            let provider = self.runtime.task_host().workflow_stream();
            wire_api::replay_history(
                &provider,
                &self.verifier,
                &self.verifier.audience().id,
                request,
            )
            .await
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
