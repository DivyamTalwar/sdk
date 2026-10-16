#[path = "../../../sdk-docs/src/path_identity.rs"]
mod identity;
#[cfg(test)]
use identity::components;
pub(super) use identity::rebase;
pub(super) use identity::stable_filename;

#[cfg(test)]
proptest::proptest! {
    #[test]
    fn receipt_paths_rebase_components_across_windows_spellings(
        directory in "[a-z]{1,12}", leaf in "[a-z]{1,12}", drive in b'A'..=b'Z',
    ) {
        let drive = char::from(drive);
        for (old, variants, new) in [
            (format!("{drive}:\\old\\{directory}"), vec![
                format!("{drive}:\\old\\{directory}\\{leaf}"),
                format!(r"\\?\{drive}:\OLD\{}\{leaf}", directory.to_ascii_uppercase()),
                format!("{}:/OLD/{directory}/{leaf}", drive.to_ascii_lowercase()),
            ], format!("{drive}:\\new\\é")),
            (format!(r"\\server\share\old\{directory}"), vec![
                format!(r"\\SERVER\SHARE\OLD\{directory}\{leaf}"),
                format!(r"\\?\UNC\server\share\OLD\{directory}\{leaf}"),
                format!("//server/share/old/{directory}/{leaf}"),
            ], r"\\server\share\new\é".to_owned()),
        ] {
            let mut expected = components(&new, true).unwrap();
            expected.push(leaf.clone());
            for path in variants {
                let result = rebase(&path, &old, &new, true).unwrap();
                proptest::prop_assert_eq!(components(&result, true).unwrap(), expected.clone());
                proptest::prop_assert_eq!(stable_filename(&path, &old), stable_filename(&result, &new));
                proptest::prop_assert_eq!(rebase(&result, &new, &old, true).and_then(|p| components(&p, true)), components(&format!("{old}/{leaf}"), true));
            }
            let sibling = format!("{old}-sibling/{leaf}");
            proptest::prop_assert!(rebase(&sibling, &old, &new, true).is_none());
        }
        proptest::prop_assert!(rebase("/OLD/item", "/old", "/new", false).is_none());
        proptest::prop_assert!(rebase("/old/../other", "/old", "/new", false).is_none());
    }
}
