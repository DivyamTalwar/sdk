use std::future::Future;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub(super) const READY: &str = "ACYCLIC_METADATA_READY";

pub(super) async fn read_output(
    reader: &mut (impl AsyncBufRead + Unpin),
    line: &mut String,
    stop_at_ready: bool,
    mut output: impl FnMut(&str),
) -> io::Result<bool> {
    while reader.read_line(line).await? != 0 {
        let ready = line.trim_end() == READY;
        if !ready {
            output(line);
        }
        line.clear();
        if ready && stop_at_ready {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) async fn after_ready<E: From<io::Error>>(
    ready: impl Future<Output = io::Result<()>>,
    before_action: impl FnOnce() -> Result<(), E>,
    go: impl FnOnce() -> io::Result<()>,
) -> Result<(), E> {
    ready.await?;
    before_action()?;
    go()?;
    Ok(())
}
