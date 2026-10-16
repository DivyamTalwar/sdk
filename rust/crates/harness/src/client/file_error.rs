use crate::Error;

/// Submission backpressure is distinct from malformed input and provider failure.
#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(tag = "kind", content = "detail")]
#[cfg_attr(feature = "wasm", derive(tsify::Tsify))]
#[cfg_attr(feature = "wasm", tsify(large_number_types_as_bigints))]
pub enum FileClientError {
    /// A definite target rejection was checkpointed and retired from the outbox.
    #[error("request {request:?} rejected: {reason}")]
    Rejected {
        /// Immutable request identity.
        request: crate::task_volume::FileKey,
        /// Target diagnostic, without persisted credentials or command bodies.
        reason: String,
    },
    /// The pending command count has reached its configured bound.
    #[error("outbox command quota reached ({limit})")]
    CommandQuota {
        /// Configured maximum pending commands.
        limit: usize,
    },
    /// The pending bytes plus this request exceed the configured bound.
    #[error("outbox byte quota reached ({limit})")]
    ByteQuota {
        /// Configured maximum pending payload bytes.
        limit: u64,
    },
    /// Input, history, identity or provider failure, retaining its classification.
    #[error(transparent)]
    Harness(
        #[from]
        #[serde(serialize_with = "diagnostic")]
        #[cfg_attr(feature = "wasm", tsify(type = "string"))]
        Error,
    ),
}

fn diagnostic<S: serde::Serializer>(error: &Error, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(error)
}
