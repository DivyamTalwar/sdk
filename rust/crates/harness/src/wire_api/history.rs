use crate::{
    Error, Result,
    core::AuthorityVerifier,
    store::HistoryReader,
    wire,
    wire_codec::{encode_authority, encode_event_envelope},
};
use acyclic_stream::{BoxProviderFuture, BoxProviderStream, StreamProvider};
use futures::StreamExt as _;
use std::sync::Arc;

/// Adapts the authoritative signed history to gapless wire deliveries.
/// Every adapter uses this same cursor and signature verification path.
pub fn replay_history<'a, P: StreamProvider>(
    provider: &'a Arc<P>,
    verifier: &'a AuthorityVerifier,
    generation: &'a str,
    request: wire::ResumeRequest,
) -> BoxProviderFuture<'a, Result<BoxProviderStream<'static, Result<wire::Delivery>>>> {
    Box::pin(async move {
        super::validate_resume_protocol(&request)?;
        if request.cursors.is_empty() {
            return Err(Error::Invalid("replay requires an explicit cursor".into()));
        }
        let expected_authority = encode_authority(verifier.audience());
        let mut streams = Vec::with_capacity(request.cursors.len());
        for cursor in request.cursors {
            if cursor.authority.as_ref() != Some(&expected_authority) {
                return Err(Error::Unauthorized(
                    "replay authority belongs to another workspace".into(),
                ));
            }
            if cursor.generation != generation {
                return Err(Error::Conflict("local replay generation changed".into()));
            }
            let reader = HistoryReader::new(provider, verifier.audience(), verifier, &mut None)?;
            // Pin validates the resume offset before opening a live, gapless producer.
            let pinned = reader.pin(cursor.revision, None).await?;
            let records = provider
                .follow(
                    acyclic_stream::StreamPath::new(verifier.audience().stream_path(&mut None)?)
                        .map_err(storage)?,
                    cursor.revision,
                )
                .await
                .map_err(storage)?;
            let verifier = verifier.clone();
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
            streams.push(Box::pin(stream) as BoxProviderStream<'static, Result<wire::Delivery>>);
        }
        Ok(Box::pin(futures::stream::select_all(streams))
            as BoxProviderStream<'static, Result<wire::Delivery>>)
    })
}
fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}
