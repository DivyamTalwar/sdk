use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn pure_args(args: &[String]) -> bool {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    if args == ["-vV"] {
        return true;
    }
    let Some(mut rest) = args.strip_prefix(&["-", "--crate-name", "___", "--print=file-names"])
    else {
        return false;
    };
    if let Some(tail) = rest.strip_prefix(&["-C", "target-feature=+crt-static"]) {
        rest = tail;
    }
    if rest.first() == Some(&"--target") {
        if !matches!(
            rest.get(1),
            Some(
                &"x86_64-pc-windows-msvc"
                    | &"x86_64-pc-windows-gnu"
                    | &"x86_64-unknown-linux-gnu"
                    | &"aarch64-apple-darwin"
            )
        ) {
            return false;
        }
        rest = &rest[2..];
    }
    rest == [
        "--crate-type",
        "bin",
        "--crate-type",
        "rlib",
        "--crate-type",
        "dylib",
        "--crate-type",
        "cdylib",
        "--crate-type",
        "staticlib",
        "--crate-type",
        "proc-macro",
        "--print=sysroot",
        "--print=split-debuginfo",
        "--print=crate-name",
        "--print=cfg",
        "-Wwarnings",
    ]
}

pub fn encode(key: &str, value: &str) -> Vec<u8> {
    serde_json::to_vec(&(key, value, digest(value.as_bytes()))).unwrap()
}

pub fn decode(bytes: &[u8], key: &str) -> Option<String> {
    if bytes.len() > 65536 {
        return None;
    }
    let (actual, value, checksum): (String, String, String) = serde_json::from_slice(bytes).ok()?;
    (actual == key && digest(value.as_bytes()) == checksum).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn only_complete_standard_target_queries_are_cached(target in 0usize..5, crt in any::<bool>()) {
            let targets = [None, Some("x86_64-pc-windows-msvc"), Some("x86_64-pc-windows-gnu"), Some("x86_64-unknown-linux-gnu"), Some("aarch64-apple-darwin")];
            let mut args = ["-", "--crate-name", "___", "--print=file-names"].map(str::to_owned).to_vec();
            if crt { args.extend(["-C".into(), "target-feature=+crt-static".into()]); }
            if let Some(target) = targets[target] { args.extend(["--target".into(), target.into()]); }
            args.extend("--crate-type bin --crate-type rlib --crate-type dylib --crate-type cdylib --crate-type staticlib --crate-type proc-macro --print=sysroot --print=split-debuginfo --print=crate-name --print=cfg -Wwarnings".split_whitespace().map(str::to_owned));
            prop_assert!(pure_args(&args));
            args[0] = "/source.rs".into();
            prop_assert!(!pure_args(&args), "source-reading compiler commands cannot be memoized");
        }
        #[test]
        fn probe_records_round_trip_and_bind_key_and_every_output_byte(key in ".{0,32}", value in ".{0,512}") {
            let encoded = encode(&key, &value);
            prop_assert_eq!(decode(&encoded, &key), Some(value.clone()));
            let other_key = format!("{key}!");
            prop_assert!(decode(&encoded, &other_key).is_none());
            let corrupted = serde_json::to_vec(&(&key, format!("{value}!"), digest(value.as_bytes()))).unwrap();
            prop_assert!(decode(&corrupted, &key).is_none());
        }
        #[test]
        fn compiler_probe_argv_round_trips_and_never_accepts_extra_arguments(extra in ".{0,32}") {
            let original = vec!["-vV".to_owned()];
            let replay: Vec<String> = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
            prop_assert!(pure_args(&replay));
            prop_assert!(!pure_args(&["-vV".into(), extra]));
            prop_assert!(!pure_args(&["--print=cfg".into(), "--target".into(), "/custom.json".into()]));
        }
    }
}
