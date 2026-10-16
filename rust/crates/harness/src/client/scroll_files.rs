//! Transcript navigation is an ordinary followed layout-file change.
use super::{CommandFileRoot, CommandSession};
use crate::{Error, Result};
use acyclic_stream::StreamProvider;

/// Rows retained above the followed transcript tail. Missing state follows tail.
pub fn tui_scroll<P: StreamProvider + ?Sized>(files: &CommandSession<P>) -> Result<usize> {
    let layout = files.state(CommandFileRoot::Session, "layout.json")?;
    let Some(value) = layout
        .as_ref()
        .and_then(|layout| layout.get("transcript_back"))
    else {
        return Ok(0);
    };
    let rows = value
        .as_u64()
        .ok_or_else(|| Error::Invalid("transcript_back must be a row count".into()))?;
    usize::try_from(rows).map_err(|error| Error::Invalid(error.to_string()))
}

/// Publish a page movement bounded by the actual rendered transcript viewport.
pub async fn scroll_tui<P: StreamProvider + ?Sized>(
    files: &mut CommandSession<P>,
    up: bool,
    page_rows: usize,
    maximum: usize,
) -> Result<()> {
    let back = tui_scroll(files)?;
    let current = back.min(maximum);
    let next = if up {
        current.saturating_add(page_rows).min(maximum)
    } else {
        current.saturating_sub(page_rows)
    };
    if next != back {
        let mut layout = files
            .state(CommandFileRoot::Session, "layout.json")?
            .unwrap_or_else(|| serde_json::json!({}));
        layout["transcript_back"] = serde_json::json!(next);
        files
            .write_state(CommandFileRoot::Session, "layout.json", &layout)
            .await?;
    }
    Ok(())
}
