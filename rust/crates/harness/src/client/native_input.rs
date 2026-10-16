//! Native input adapters; wire decoding and settlement stay in the shared runner.
use crate::{Error, Result};
use futures::{StreamExt, stream::BoxStream};

/// Bounded JSONL stdin, independently scheduled from replay/follow.
pub fn stdin_jsonl() -> BoxStream<'static, Result<String>> {
    Box::pin(
        tokio_util::codec::FramedRead::new(
            tokio::io::stdin(),
            tokio_util::codec::LinesCodec::new_with_max_length(65_536),
        )
        .map(|line| line.map_err(|error| Error::Invalid(error.to_string()))),
    )
}

/// Waits for UI input or a validated delivery. Publication/commit is completed
/// outside select, so an input event cannot cancel a half-published delivery.
#[cfg(feature = "tui")]
pub async fn next_tui_event<S: super::ClientStateStore>(
    client: &mut super::HarnessClient<S>,
    input: &mut (impl futures::Stream<Item = Result<crate::tui::key_event::InputEvent>> + Unpin),
    completions: &mut crate::tui_files::CompletionFiles,
    prompt: &mut crate::tui::PromptInput,
    commands: &mut super::CommandSession<acyclic_stream::LocalStream>,
    documents: &mut acyclic_stream::BoxProviderStream<'static, Result<super::ViewUpdate>>,
) -> Result<Option<crate::tui::key_event::InputEvent>> {
    let delivery = tokio::select! {
        event = input.next() => return event.transpose()?.map(Some).ok_or_else(|| Error::Storage("terminal input closed".into())),
        delivery = client.read_delivery() => delivery?,
        update = documents.next() => {
            client.apply_update(update.ok_or_else(|| Error::Conflict("document follow ended; reopen snapshots".into()))??)?;
            return Ok(None);
        },
        change = commands.observe_change() => { change?; return Ok(None); },
        change = completions.read_change() => {
            completions.apply_change(change?, prompt)?;
            return Ok(None);
        },
    };
    let delivery = delivery.ok_or_else(|| {
        Error::Conflict("live replay ended; reconnect from the committed cursor".into())
    })?;
    client.publish_delivery(delivery, |_| Ok(())).await?;
    Ok(None)
}
