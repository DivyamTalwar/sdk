use super::*;
use proptest::prelude::*;
use tokio_util::codec::Decoder as _;

proptest! {
    // Property: arbitrary workspace, method and payload survive every stream
    // split without early delivery, truncation or an extra decoded frame.
    #[test]
    fn call_round_trip_survives_fragmentation(
        workspace in ".{0,48}",
        method in any::<i32>(),
        payload in prop::collection::vec(any::<u8>(), 0..128),
        split in any::<usize>(),
    ) {
        let request = Request { workspace, method, payload: payload.into() };
        let bytes = request.encode_to_vec();
        let mut encoded = bytes::BytesMut::new();
        encoded.put_u32(u32::try_from(bytes.len())?);
        encoded.extend_from_slice(&bytes);
        let split = split % (encoded.len() + 1);
        let mut pending = encoded.split_to(split);
        let mut codec = codec();
        let first = codec.decode(&mut pending)?;
        if !encoded.is_empty() {
            prop_assert!(first.is_none());
            pending.extend_from_slice(&encoded);
        }
        let frame = match first {
            Some(frame) => frame,
            None => codec.decode(&mut pending)?.ok_or_else(|| TestCaseError::fail("missing complete frame"))?,
        };
        prop_assert_eq!(Request::decode(frame)?, request);
        prop_assert!(codec.decode(&mut pending)?.is_none());
    }

    // Property: any accepted protobuf message is stable under canonical
    // encoding, including unknown tags and noncanonical field ordering.
    #[test]
    fn accepted_untrusted_request_has_a_stable_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(request) = Request::decode(bytes.as_slice()) {
            prop_assert_eq!(Request::decode(request.encode_to_vec().as_slice())?, request);
        }
        if let Ok(response) = Response::decode(bytes.as_slice()) {
            prop_assert_eq!(Response::decode(response.encode_to_vec().as_slice())?, response);
        }
    }

    #[test]
    fn oversized_prefix_is_rejected_without_a_body(extra in 1_u32..1024) {
        let mut input = bytes::BytesMut::new();
        input.put_u32(u32::try_from(MAX_FRAME_BYTES)? + extra);
        let mut codec = codec();
        prop_assert!(codec.decode(&mut input).is_err());
    }
}
