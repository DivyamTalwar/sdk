use super::*;
use proptest::prelude::*;

proptest! {
    // A physical rebase preserves the real normalized digest for every Windows
    // spelling, even when the test host cannot open any of these source paths.
    #[test]
    fn windows_receipts_preserve_digest_identity_on_every_host(
        directory in "[a-z]{1,10}", leaf in "[a-z]{1,10}", drive in b'A'..=b'Z',
    ) {
        let drive = char::from(drive);
        for (old, path, new) in [
            (format!("{drive}:\\old\\{directory}"), format!(r"\\?\{drive}:\OLD\{directory}\src\{leaf}.rs"), "D:\\new\\checkout".to_owned()),
            (format!(r"\\server\share\old\{directory}"), format!(r"\\?\UNC\SERVER\SHARE\old\{directory}\src\{leaf}.rs"), r"\\server\share\new\checkout".to_owned()),
        ] {
            let rebase = crate::path_identity::rebase(&path, &old, &new, true).unwrap();
            let normalize = |path: &str, root: &str| normalized_rustdoc_digest_bytes(
                &serde_json::to_vec(&serde_json::json!({"index": {"0": {"span": {"filename": path}}}})).unwrap(), Path::new(root)).unwrap();
            prop_assert_eq!(normalize(&path, &old), normalize(&rebase, &new));
            prop_assert_ne!(normalize(&path, &old), normalize(&format!("{rebase}.changed"), &new));
        }
    }
}
