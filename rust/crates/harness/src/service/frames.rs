use super::storage;
use crate::{Error, Result, wire};
use bytes::{BufMut as _, Bytes};
use futures::StreamExt as _;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

mod protocol;
pub(super) use protocol::*;

pub(super) trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}
pub(super) type Connection = Framed<Box<dyn Socket>, LengthDelimitedCodec>;

pub(super) fn connection(socket: impl Socket + 'static) -> Connection {
    Framed::new(Box::new(socket) as Box<dyn Socket>, codec())
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

#[cfg(test)]
mod properties;

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
