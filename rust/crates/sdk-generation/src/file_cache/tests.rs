use super::*;
use proptest::prelude::*;
use std::cell::Cell;
use std::fs::{File, FileTimes};
use std::time::{Duration, UNIX_EPOCH};

proptest! {
    #[test]
    fn selection_is_reused_only_within_a_command(commands in proptest::collection::vec(any::<bool>(), 1..24)) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("toolchain");
        fs::write(&path, "channel").unwrap();
        let lookups = Cell::new(0);
        let mut expected = 0;
        for (index, next_command) in commands.into_iter().enumerate() {
            if index == 0 || next_command {
                begin_run();
                expected += 1;
            }
            let value = get(&path, "rustup-which:property", || {
                lookups.set(lookups.get() + 1);
                Ok("cargo".into())
            }).unwrap();
            prop_assert_eq!(value, "cargo");
            prop_assert_eq!(lookups.get(), expected);
        }
    }

    // Property: probe keys cover every observable input and ignore env iteration order.
    #[test]
    fn cargo_probe_context_is_complete_and_order_independent(
        inputs in proptest::collection::btree_map("[A-Z]{1,8}", ".{0,16}", 0..12),
        root in "[a-z]{1,8}",
        discovery in ".{0,16}",
    ) {
        use crate::toolchains::cargo_context;
        let environment = || inputs.iter().map(|(key, value)| (key.into(), value.into()));
        let path = PathBuf::from(&root);
        let context = cargo_context(&path, &discovery, environment());
        prop_assert_eq!(&context, &cargo_context(&path, &discovery, environment().rev()));
        prop_assert_ne!(&context, &cargo_context(&path.join("different"), &discovery, environment()));
        prop_assert_ne!(&context, &cargo_context(&path, &format!("{discovery}!"), environment()));
        let changed = environment().chain([("NEW_INPUT".into(), "value".into())]);
        prop_assert_ne!(context, cargo_context(&path, &discovery, changed));
    }

    // Property: cached results equal fresh reads after every edit, replacement,
    // context transition and deletion; repeated unchanged reads do no I/O work.
    #[test]
    fn follows_file_and_context_changes(
        initial in proptest::collection::vec(any::<u8>(), 1..128),
        operations in proptest::collection::vec((0u8..5, any::<u8>()), 1..24),
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        let mut bytes = initial;
        let mut context = 0;
        fs::write(&path, &bytes).unwrap();
        let reads = Cell::new(0);
        for (step, (operation, byte)) in operations.into_iter().enumerate() {
            match operation {
                0 => {},
                1 => {
                    bytes[0] ^= byte | 1;
                    fs::write(&path, &bytes).unwrap();
                    // Give every rewrite a distinct mtime even on coarse filesystems.
                    File::options().write(true).open(&path).unwrap().set_times(
                        FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(step as u64 + 1))
                    ).unwrap();
                },
                2 => {
                    let modified = fs::metadata(&path).unwrap().modified().unwrap();
                    bytes[0] ^= byte | 1;
                    let replacement = directory.path().join("replacement");
                    fs::write(&replacement, &bytes).unwrap();
                    File::options().write(true).open(&replacement).unwrap().set_times(
                        FileTimes::new().set_modified(modified)
                    ).unwrap();
                    fs::remove_file(&path).unwrap();
                    fs::rename(replacement, &path).unwrap();
                },
                3 => context = byte,
                4 => {
                    fs::remove_file(&path).unwrap();
                    prop_assert!(get(&path, "deleted", || panic!("missing file was read")).is_err());
                    bytes.push(byte);
                    fs::write(&path, &bytes).unwrap();
                },
                _ => unreachable!(),
            }
            let context = context.to_string();
            let load = || {
                reads.set(reads.get() + 1);
                Ok(format!("{}:{context}", crate::sha256_file(&path)?))
            };
            let expected = format!("{}:{context}", crate::sha256_bytes(&bytes));
            prop_assert_eq!(get(&path, &context, load).unwrap(), expected.clone());
            let before = reads.get();
            prop_assert_eq!(get(&path, &context, load).unwrap(), expected);
            prop_assert_eq!(reads.get(), before);
        }
    }
}

#[test]
fn cargo_probe_key_frames_environment_names_and_values() {
    use crate::toolchains::cargo_context;
    let context =
        |key: &str, value: &str| cargo_context(Path::new("root"), "", [(key.into(), value.into())]);
    assert_ne!(context("A", "BC"), context("AB", "C"));
}

#[test]
fn each_command_refreshes_selection_and_retains_unchanged_file_identities() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("toolchain");
    fs::write(&path, "channel").unwrap();
    let lookups = Cell::new(0);
    let selection = || {
        get(&path, "rustup-which:context", || {
            lookups.set(lookups.get() + 1);
            Ok("cargo".into())
        })
        .unwrap()
    };
    assert_eq!(selection(), "cargo");
    assert_eq!(selection(), "cargo");
    assert_eq!(lookups.get(), 1);
    get(&path, "sha256", || Ok("digest".into())).unwrap();
    crate::run(["help".into()]).unwrap();
    get(&path, "sha256", || {
        panic!("unchanged file identity was discarded")
    })
    .unwrap();
    assert_eq!(selection(), "cargo");
    assert_eq!(lookups.get(), 2);
}

#[test]
fn changes_during_reads_and_failed_reads_are_not_cached() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input");
    fs::write(&path, b"before").unwrap();
    let error = get(&path, "mutation", || {
        let value = crate::sha256_file(&path)?;
        fs::write(&path, b"after with different length").unwrap();
        Ok(value)
    })
    .unwrap_err();
    assert!(error.0.contains("file changed while reading identity"));
    assert_eq!(
        sha256_file(&path).unwrap(),
        crate::sha256_file(&path).unwrap()
    );
    assert!(get(&path, "failure", || Err(CliError("unavailable".into()))).is_err());
    assert_eq!(
        get(&path, "failure", || Ok("retried".into())).unwrap(),
        "retried"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn same_length_edit_with_restored_mtime_invalidates_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tool");
    fs::write(&path, b"before").unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let original = sha256_file(&path).unwrap();
    fs::write(&path, b"edited").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    assert_ne!(sha256_file(&path).unwrap(), original);
    assert_eq!(
        sha256_file(&path).unwrap(),
        crate::sha256_file(&path).unwrap()
    );
}
