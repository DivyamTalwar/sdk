#![no_main]
use libfuzzer_sys::fuzz_target;
use prost::Message;

fuzz_target!(|bytes: &[u8]| {
    let generated = acyclic_stream::wire::SessionFrame {
        channel_id: bytes.first().copied().unwrap_or_default() as u64,
        body: Some(acyclic_stream::wire::session_frame::Body::Window(acyclic_stream::wire::SessionWindow { bytes: bytes.len() as u64, frames: 1 })),
        ..Default::default()
    };
    assert_eq!(acyclic_stream::wire_codec::session::decode_frame(&generated.encode_to_vec()).unwrap(), generated);
    if let Ok(frame) = acyclic_stream::wire_codec::session::decode_frame(bytes) {
        let canonical = frame.encode_to_vec();
        assert_eq!(acyclic_stream::wire_codec::session::decode_frame(&canonical).unwrap(), frame);
    }
});
