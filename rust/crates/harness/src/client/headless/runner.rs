//! The shared headless policy entry points accept native file or injected wire clients.
use super::{ClientStateStore, HarnessClient, HeadlessOutcome};
use crate::{Result, wire};
use futures::Stream;
use std::{future::Future, io::Write};

/// A client backend for the shared headless runner. Native clients use task files;
/// injected transports retain the descriptor-based service frame contract.
pub trait HeadlessClient {
    /// Durable command accepted by this backend.
    type Command;
    /// Exact operation selection, fixed before submission.
    type Target;
    /// Runs one prompt while publishing its followed records.
    fn prompt<W: Write>(
        &mut self,
        command: Self::Command,
        target: Self::Target,
        output: &mut W,
    ) -> impl Future<Output = Result<HeadlessOutcome>>;
    /// Runs bounded JSONL input alongside the same followed records.
    fn input<W: Write>(
        &mut self,
        target: Self::Target,
        input: impl Stream<Item = Result<String>> + Unpin,
        output: &mut W,
    ) -> impl Future<Output = Result<HeadlessOutcome>>;
}

/// Submits through the client and follows its exact operation to settlement.
pub async fn run_headless<C: HeadlessClient>(
    client: &mut C,
    command: C::Command,
    target: C::Target,
    output: &mut impl Write,
) -> Result<HeadlessOutcome> {
    client.prompt(command, target, output).await
}

/// Reads this client's canonical JSONL input and drains followed output before exit.
pub async fn run_jsonl<C: HeadlessClient>(
    client: &mut C,
    target: C::Target,
    input: impl Stream<Item = Result<String>> + Unpin,
    output: &mut impl Write,
) -> Result<HeadlessOutcome> {
    client.input(target, input, output).await
}

impl<S: ClientStateStore> HeadlessClient for HarnessClient<S> {
    type Command = wire::CommandEnvelope;
    type Target = wire::ObserveRequest;
    async fn prompt<W: Write>(
        &mut self,
        command: Self::Command,
        target: Self::Target,
        output: &mut W,
    ) -> Result<HeadlessOutcome> {
        super::wire_headless(self, command, target, output).await
    }
    async fn input<W: Write>(
        &mut self,
        target: Self::Target,
        input: impl Stream<Item = Result<String>> + Unpin,
        output: &mut W,
    ) -> Result<HeadlessOutcome> {
        super::wire_jsonl(self, target, input, output).await
    }
}
