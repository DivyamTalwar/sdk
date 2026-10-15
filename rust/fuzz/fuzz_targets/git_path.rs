#![no_main]

use acyclic_harness::Error;
use libfuzzer_sys::fuzz_target;
use std::path::{Component, Path};

// Compile the production parser, with the production Error type, without
// exporting a second public facade entry point just for verification.
#[path = "../../crates/harness/src/tools/git/path.rs"]
mod path;

fuzz_target!(|bytes: &[u8]| {
    let Ok(argument) = std::str::from_utf8(bytes) else {
        return;
    };
    let root = Path::new("/workspace");
    if let Ok(parsed) = path::workspace_path_argument(root, argument) {
        assert!(!parsed.contains('\0'));
        assert!(parsed.starts_with('/'));
        assert!(
            Path::new(&parsed)
                .components()
                .all(|part| !matches!(part, Component::ParentDir))
        );
        let absolute = root.join(parsed.trim_start_matches('/'));
        assert_eq!(
            path::workspace_path_argument(root, absolute.to_str().unwrap()).unwrap(),
            parsed
        );
    }
});
