use crate::wire;
use bytes::Bytes;
use prost::Message;
use tokio_util::codec::LengthDelimitedCodec;

// A delivery contains one stored EventEnvelope, a repeated authority and its
// generation ID. Each is bounded by the authenticated record containing it.
// Each protobuf field needs at most one tag byte and ten length/varint bytes;
// the two revision fields and live flag plus the RPC payload wrap are extra.
const FIELD_OVERHEAD: usize = 1 + 10;
const MAX_DELIVERY_BYTES: usize =
    3 * (acyclic_stream::MAX_RECORD_BYTES + FIELD_OVERHEAD) + 2 * FIELD_OVERHEAD + 2;
pub(crate) const MAX_FRAME_BYTES: usize = MAX_DELIVERY_BYTES + FIELD_OVERHEAD;

// One RPC per connection: Replay owns its connection until the consumer drops it.
// The payload is always an existing acyclic.harness.v2 message.
#[derive(Clone, PartialEq, Message)]
pub(crate) struct Request {
    #[prost(string, tag = "1")]
    pub workspace: String,
    #[prost(enumeration = "Method", tag = "2")]
    pub method: i32,
    #[prost(bytes = "bytes", tag = "3")]
    pub payload: Bytes,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct Call<M: Message + Default + std::fmt::Debug> {
    #[prost(string, tag = "1")]
    pub workspace: String,
    #[prost(enumeration = "Method", tag = "2")]
    pub method: i32,
    #[prost(message, optional, tag = "3")]
    pub payload: Option<M>,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct Reply<M: Message + Default + std::fmt::Debug> {
    #[prost(message, optional, tag = "1")]
    pub payload: Option<M>,
    #[prost(message, optional, tag = "2")]
    pub error: Option<wire::Error>,
    #[prost(bool, tag = "3")]
    pub end: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub(crate) enum Method {
    Handshake = 0,
    Submit = 1,
    Replay = 2,
    Observe = 3,
    Cancel = 4,
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct Response {
    #[prost(bytes = "bytes", tag = "1")]
    pub payload: Bytes,
    #[prost(message, optional, tag = "2")]
    pub error: Option<wire::Error>,
    #[prost(bool, tag = "3")]
    pub end: bool,
}

pub(crate) fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_length(4)
        .big_endian()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_codec()
}
