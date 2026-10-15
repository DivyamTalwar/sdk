#![no_main]

use acyclic_harness::{Error, InteractionRejection, OperationId, Result, encode_error, wire};
use libfuzzer_sys::fuzz_target;
use prost::Message as _;
use tokio_util::codec::Decoder as _;

fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

// Compile the actual private transport boundary, without exposing a fuzz API.
#[path = "../../rust/crates/harness/src/service/frames.rs"]
mod frames;

fuzz_target!(|input: &[u8]| {
    if input.len() > frames::MAX_FRAME_BYTES + 4 {
        return;
    }
    let mut codec = tokio_util::codec::LengthDelimitedCodec::builder()
        .length_field_length(4)
        .big_endian()
        .max_frame_length(frames::MAX_FRAME_BYTES)
        .new_codec();
    let mut bytes = bytes::BytesMut::from(input);
    if let Ok(Some(frame)) = codec.decode(&mut bytes) {
        if let Ok(request) = frames::Request::decode(frame.clone()) {
            assert_eq!(
                frames::Request::decode(request.encode_to_vec().as_slice())
                    .as_ref()
                    .ok(),
                Some(&request)
            );
            match frames::Method::try_from(request.method) {
                Ok(frames::Method::Handshake) => {
                    let _ = wire::HandshakeRequest::decode(request.payload);
                }
                Ok(frames::Method::Submit) => {
                    let _ = wire::CommandEnvelope::decode(request.payload);
                }
                Ok(frames::Method::Observe) => {
                    let _ = wire::ObserveRequest::decode(request.payload);
                }
                Ok(frames::Method::Cancel) => {
                    let _ = wire::CancelRequest::decode(request.payload);
                }
                Ok(frames::Method::Replay) => {
                    let _ = wire::ResumeRequest::decode(request.payload);
                }
                Err(_) => {}
            }
        }
        if let Ok(response) = frames::Response::decode(frame) {
            assert_eq!(
                frames::Response::decode(response.encode_to_vec().as_slice())
                    .as_ref()
                    .ok(),
                Some(&response)
            );
        }
    }
    if let Ok(command) = std::str::from_utf8(input) {
        let _ = acyclic_harness::tools::parse_command(command);
        let root = std::env::temp_dir().join("managed-workspace");
        if let Ok(path) = acyclic_harness::tools::git::workspace_path_argument(&root, command) {
            assert_eq!(
                acyclic_harness::tools::git::workspace_path_argument(
                    &root,
                    &root.join(path.trim_start_matches('/')).to_string_lossy(),
                )
                .as_ref()
                .ok(),
                Some(&path)
            );
        }
    }
});
