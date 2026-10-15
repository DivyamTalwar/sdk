use crate::invalid;
use acyclic_harness::Result;
use acyclic_stream::BoxProviderStream;
use bytes::Bytes;
use futures::StreamExt as _;

pub(crate) fn decode<'a>(
    mut body: BoxProviderStream<'a, Result<Bytes>>,
    maximum: usize,
) -> BoxProviderStream<'a, Result<String>> {
    Box::pin(async_stream::try_stream! {
        let mut line = Vec::new();
        let mut data = String::new();
        let mut bytes = 0usize;
        let mut cr = false;
        while let Some(chunk) = body.next().await {
            for byte in chunk? {
                if byte == b'\n' && cr { cr = false; continue; }
                cr = byte == b'\r';
                if byte != b'\n' && byte != b'\r' {
                    bytes = bytes.checked_add(1).ok_or_else(|| invalid("SSE size overflow"))?;
                    if bytes > maximum { Err(invalid("SSE event exceeds configured byte limit"))?; }
                    line.push(byte);
                    continue;
                }
                if line.is_empty() {
                    bytes = 0;
                    if !data.is_empty() {
                        data.pop();
                        yield std::mem::take(&mut data);
                    }
                } else {
                    let text = std::str::from_utf8(&line).map_err(|_| invalid("SSE is not UTF-8"))?;
                    if let Some(value) = text.strip_prefix("data:") {
                        data.push_str(value.strip_prefix(' ').unwrap_or(value));
                        data.push('\n');
                    }
                    line.clear();
                }
            }
        }
        if !line.is_empty() || !data.is_empty() { Err(acyclic_harness::Error::Storage("truncated SSE event".into()))?; }
    })
}
