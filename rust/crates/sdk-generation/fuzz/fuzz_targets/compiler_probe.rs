#![no_main]
#[path = "../../tests/support/probe_codec.rs"]
mod codec;

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Some(value) = codec::decode(bytes, "fixture") {
        assert_eq!(codec::decode(&codec::encode("fixture", &value), "fixture"), Some(value));
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        let args = text.split('\0').map(str::to_owned).collect::<Vec<_>>();
        let encoded = serde_json::to_vec(&args).unwrap();
        let replay: Vec<String> = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(codec::pure_args(&args), codec::pure_args(&replay));
        if text.len() < 8192 {
            assert_eq!(codec::decode(&codec::encode("fixture", text), "fixture").as_deref(), Some(text));
        }
    }
});
