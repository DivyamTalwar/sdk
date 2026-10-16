use crate::Result;

// A published result is not the workflow's settlement acknowledgement. Finish
// retained work before replying or attempting to fill the next input slot.
pub(super) async fn recover_before_delivery(
    initial: bool,
    answered: bool,
    recovery: impl Future<Output = Result<()>>,
) -> Result<()> {
    if !initial || answered {
        recovery.await?;
    }
    Ok(())
}
