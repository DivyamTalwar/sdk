#![no_main]

use acyclic_fs::{GitCommand, parse_git_argv};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let argv: Vec<_> = text.split('\0').map(str::to_owned).collect();
    if let Ok(command) = parse_git_argv::<std::convert::Infallible>(&argv, "agent", 0) {
        let wire = serde_json::to_vec(&command).unwrap();
        let restored: GitCommand = serde_json::from_slice(&wire).unwrap();
        assert_eq!(command, restored);
    }
});
