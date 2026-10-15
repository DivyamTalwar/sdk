use super::storage;
use crate::{Error, Result, wire};
use bytes::Bytes;
use futures::{SinkExt as _, StreamExt as _};
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

// One RPC per connection: Replay owns its connection until the consumer drops it.
// The payload is always an existing acyclic.harness.v2 message.
#[derive(Clone, PartialEq, Message)]
pub(super) struct Request {
    #[prost(string, tag = "1")]
    pub workspace: String,
    #[prost(enumeration = "Method", tag = "2")]
    pub method: i32,
    #[prost(bytes = "bytes", tag = "3")]
    pub payload: Bytes,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct Call<M: Message + Default + std::fmt::Debug> {
    #[prost(string, tag = "1")]
    pub workspace: String,
    #[prost(enumeration = "Method", tag = "2")]
    pub method: i32,
    #[prost(message, optional, tag = "3")]
    pub payload: Option<M>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct Reply<M: Message + Default + std::fmt::Debug> {
    #[prost(message, optional, tag = "1")]
    pub payload: Option<M>,
    #[prost(message, optional, tag = "2")]
    pub error: Option<wire::Error>,
    #[prost(bool, tag = "3")]
    pub end: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub(super) enum Method {
    Handshake = 0,
    Submit = 1,
    Replay = 2,
    Observe = 3,
    Cancel = 4,
    Authorize = 5,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct Response {
    #[prost(bytes = "bytes", tag = "1")]
    pub payload: Bytes,
    #[prost(message, optional, tag = "2")]
    pub error: Option<wire::Error>,
    #[prost(bool, tag = "3")]
    pub end: bool,
}

pub(super) trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}
pub(super) type Connection = Framed<Box<dyn Socket>, LengthDelimitedCodec>;

pub(super) fn connection(socket: impl Socket + 'static) -> Connection {
    LengthDelimitedCodec::builder()
        .max_frame_length(acyclic_stream::MAX_RECORD_BYTES)
        .new_framed(Box::new(socket) as Box<dyn Socket>)
}

pub(super) async fn send<M: Message + Default + std::fmt::Debug>(
    connection: &mut Connection,
    message: &M,
) -> Result<()> {
    if message.encoded_len() > acyclic_stream::MAX_RECORD_BYTES {
        return Err(Error::Invalid(
            "local wire frame exceeds the record limit".into(),
        ));
    }
    connection
        .send(Bytes::from(message.encode_to_vec()))
        .await
        .map_err(storage)
}

pub(super) async fn receive<M: Message + Default + std::fmt::Debug>(
    connection: &mut Connection,
) -> Result<M> {
    let frame = connection
        .next()
        .await
        .ok_or_else(|| {
            Error::Storage("local service disconnected before completing the RPC".into())
        })?
        .map_err(storage)?;
    M::decode(frame.freeze()).map_err(|error| Error::Invalid(error.to_string()))
}

pub(super) fn response<M: Message + Default + std::fmt::Debug>(message: M) -> Reply<M> {
    Reply {
        payload: Some(message),
        error: None,
        end: false,
    }
}

pub(super) fn error(error: &Error) -> Response {
    Response {
        error: Some(crate::encode_error(error)),
        ..Response::default()
    }
}

pub(super) fn remote(error: wire::Error) -> Error {
    use wire::ErrorCode;
    match ErrorCode::try_from(error.code) {
        Ok(ErrorCode::Invalid) => Error::Invalid(error.message),
        Ok(ErrorCode::NotFound) => Error::NotFound(error.message),
        Ok(ErrorCode::Conflict) => Error::Conflict(error.message),
        Ok(ErrorCode::Unauthorized) => Error::Unauthorized(error.message),
        Ok(ErrorCode::Unsupported) => Error::Unsupported(error.message),
        Ok(ErrorCode::Indeterminate) => match crate::OperationId::parse(&error.operation_id) {
            Ok(operation) => Error::Indeterminate(operation),
            Err(error) => error,
        },
        Ok(ErrorCode::InteractionDeclined) => {
            Error::InteractionRejected(crate::InteractionRejection::Declined)
        }
        Ok(ErrorCode::InteractionCancelled) => {
            Error::InteractionRejected(crate::InteractionRejection::Cancelled)
        }
        Ok(ErrorCode::InteractionDenied) => {
            Error::InteractionRejected(crate::InteractionRejection::Denied)
        }
        Ok(ErrorCode::InteractionExpired) => {
            Error::InteractionRejected(crate::InteractionRejection::Expired)
        }
        _ => Error::Storage(error.message),
    }
}
