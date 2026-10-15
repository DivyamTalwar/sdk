use super::*;
use crate::{
    wire,
    wire_api::{self, OperationControlRequest},
};

impl<P: StreamProvider> CoordinatorTaskHost<P> {
    /// Authenticates a decoded control scope against this durable owner.
    pub fn authorize_operation_control(&self, request: &OperationControlRequest) -> Result<()> {
        request.authenticate(&self.verifier)?;
        Ok(())
    }

    /// Observes an authenticated operation under the coordinator lock.
    pub async fn wire_observe(
        &self,
        request: wire::ObserveRequest,
    ) -> Result<wire::OperationStatus> {
        let control = wire_api::validate_observe_request(&request)?;
        control.authenticate(&self.verifier)?;
        let mut coordinator = self.coordinator.lock().await;
        coordinator.refresh(&mut None).await?;
        let state = coordinator.observe_operation(
            &control.owner,
            &control.scope,
            &self.verifier,
            control.operation_id,
            &mut None,
        )?;
        Ok(wire_api::operation_status(state))
    }

    /// Cancels an authenticated operation with the caller's retry identity.
    pub async fn wire_cancel(&self, request: wire::CancelRequest) -> Result<wire::CancelResponse> {
        let control = wire_api::validate_cancel_request(&request)?;
        control.control.authenticate(&self.verifier)?;
        let (_, state) = self
            .coordinator
            .lock()
            .await
            .cancel_operation(
                &control.control.owner,
                &control.control.scope,
                &self.verifier,
                control.control.operation_id,
                control.idempotency_key.clone(),
                control.recursive,
                None,
                &mut None,
            )
            .await?;
        Ok(wire::CancelResponse {
            status: Some(wire_api::operation_status(&state)),
            operation: Some(wire::OperationIdentity {
                operation_id: control.control.operation_id.to_string(),
                idempotency_key: control.idempotency_key.0,
            }),
        })
    }
}
