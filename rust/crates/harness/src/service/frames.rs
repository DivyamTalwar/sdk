use super::storage;
use crate::{Error, Result, wire};
use bytes::{BufMut as _, Bytes};
use futures::StreamExt as _;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

// A delivery contains one stored EventEnvelope, a repeated authority and its
// generation ID. Each is bounded by the authenticated record containing it.
// Each protobuf field needs at most one tag byte and ten length/varint bytes;
// the two revision fields and live flag plus the RPC payload wrap are extra.
const FIELD_OVERHEAD: usize = 1 + 10;
const MAX_DELIVERY_BYTES: usize =
    3 * (acyclic_stream::MAX_RECORD_BYTES + FIELD_OVERHEAD) + 2 * FIELD_OVERHEAD + 2;
const MAX_FRAME_BYTES: usize = MAX_DELIVERY_BYTES + FIELD_OVERHEAD;

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
        .length_field_length(4)
        .big_endian()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_framed(Box::new(socket) as Box<dyn Socket>)
}

pub(super) async fn send<M: Message + Default + std::fmt::Debug>(
    connection: &mut Connection,
    message: &M,
) -> Result<()> {
    let length = message.encoded_len();
    if length > MAX_FRAME_BYTES {
        return Err(Error::Invalid(
            "local wire frame exceeds the transport envelope bound".into(),
        ));
    }
    let encoded_length = u32::try_from(length).map_err(storage)?;
    let buffer = connection.write_buffer_mut();
    buffer.reserve(4 + length);
    buffer.put_u32(encoded_length);
    message.encode_raw(buffer);
    futures::SinkExt::<Bytes>::flush(connection)
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
