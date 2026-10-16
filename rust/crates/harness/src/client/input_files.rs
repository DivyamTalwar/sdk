//! TUI intents and drafts use the command session's ordinary file publications.
use super::{CommandFileRoot, CommandRequest, CommandResolution, CommandSession};
use crate::{
    Error, Result,
    conversation::ContentResidencyVerifier,
    tui::{InputEffect, PromptInput},
};
use acyclic_stream::StreamProvider;
use serde_json::Value;

/// Native and embedded clients share classification and file publication.
pub async fn apply_tui_input<P: StreamProvider + ?Sized>(
    files: &mut CommandSession<P>,
    reader: &dyn ContentResidencyVerifier,
    effect: InputEffect,
) -> Result<()> {
    match effect {
        InputEffect::Submit {
            text, edit: None, ..
        } => {
            files.submit(&text, reader).await?;
        }
        InputEffect::Quit => {
            files
                .publish_resolution(CommandResolution::Submit {
                    request: CommandRequest::Quit,
                })
                .await?;
        }
        // TODO(#495): bind interruption, message replacement and native clipboard/editor
        // request files to the execution owner; never turn them into chat.
        _ => {
            return Err(Error::Unsupported(
                "TODO #495: native UI effect file binding".into(),
            ));
        }
    }
    Ok(())
}

/// Load one session's persisted prompt before accepting input.
pub fn restore_tui_prompt<P: StreamProvider + ?Sized>(
    files: &CommandSession<P>,
    input: &mut PromptInput,
) -> Result<()> {
    let draft = files.state(CommandFileRoot::Session, "draft.json")?;
    let text = draft
        .as_ref()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| Error::Invalid("draft file must contain a string".into()))
        })
        .transpose()?;
    let history = files
        .state(CommandFileRoot::Config, "history.json")?
        .map(|value| {
            crate::tui::parse_history(&value.to_string())
                .map_err(|error| Error::Invalid(error.to_string()))
        })
        .transpose()?;
    if let Some(text) = text {
        input.editor.set_text(text, 0);
    }
    if let Some(history) = history {
        input.replace_history(history);
    }
    // TODO(#495): connect prompt/history follow updates and the shared history writer.
    Ok(())
}

/// Persist only changed draft text, and render after the file's accepted cut.
pub async fn save_tui_draft<P: StreamProvider + ?Sized>(
    files: &mut CommandSession<P>,
    input: &PromptInput,
) -> Result<()> {
    let draft = Value::String(input.editor.text().to_owned());
    if files
        .state(CommandFileRoot::Session, "draft.json")?
        .as_ref()
        != Some(&draft)
    {
        files
            .write_state(CommandFileRoot::Session, "draft.json", &draft)
            .await?;
    }
    Ok(())
}
