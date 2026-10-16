use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Seek, Write},
    path::Path,
    process::{Command, Output},
};
#[path = "probe_codec.rs"]
pub mod codec;
#[path = "probe_fence.rs"]
mod probe_fence;

fn key(program: &Path, stamp: &impl serde::Serialize, args: &[OsString]) -> io::Result<String> {
    let bytes = serde_json::to_vec(&(
        program.as_os_str().as_encoded_bytes(),
        stamp,
        args.iter()
            .map(|arg| arg.as_encoded_bytes())
            .collect::<Vec<_>>(),
    ))
    .map_err(io::Error::other)?;
    Ok(codec::digest(&bytes))
}

fn source(input: &mut impl Read) -> io::Result<Option<fs::File>> {
    let mut first = [0];
    if input.read(&mut first)? == 0 {
        return Ok(None);
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(&first)?;
    io::copy(input, &mut file)?;
    file.rewind()?;
    Ok(Some(file))
}

pub fn cached<S: serde::Serialize + Eq>(
    directory: &Path,
    program: &Path,
    args: &[OsString],
    observe: impl Fn() -> io::Result<S>,
    query: impl FnOnce() -> io::Result<Output>,
) -> io::Result<Output> {
    let Ok(before) = observe() else {
        return query();
    };
    let Ok(key) = key(program, &before, args) else {
        return query();
    };
    let path = directory.join(&key);
    let unchanged = || {
        if observe()? == before {
            Ok(())
        } else {
            Err(io::Error::other("concrete compiler changed during probe"))
        }
    };
    let lock = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{key}.lock")))
    {
        Ok(lock) => lock,
        Err(_) => {
            let output = query()?;
            unchanged()?;
            return Ok(output);
        }
    };
    // Leaf: one cache lock, then file I/O or the concrete compiler. It cannot
    // dispatch back into this wrapper. The production lock primitive is modeled.
    probe_fence::query::<_, _, true>(
        &lock,
        &before,
        &observe,
        || {
            let value = fs::File::open(&path).ok().and_then(|file| {
                let mut bytes = Vec::new();
                file.take(65537).read_to_end(&mut bytes).ok()?;
                codec::decode(&bytes, &key)
            });
            Ok(value.map(|value| {
                #[cfg(unix)]
                use std::os::unix::process::ExitStatusExt;
                #[cfg(windows)]
                use std::os::windows::process::ExitStatusExt;
                Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: value.into_bytes(),
                    stderr: Vec::new(),
                }
            }))
        },
        query,
        |output| {
            if output.status.success() && output.stderr.is_empty() && output.stdout.len() < 8192 {
                if let Ok(value) = std::str::from_utf8(&output.stdout) {
                    let mut file = tempfile::NamedTempFile::new_in(directory)?;
                    file.write_all(&codec::encode(&key, value))?;
                    file.persist(&path).map_err(|error| error.error)?;
                }
            }
            Ok(())
        },
    )
}

pub fn run(arguments: &[OsString]) -> io::Result<i32> {
    let (program, args) = arguments
        .split_first()
        .ok_or_else(|| io::Error::other("compiler path missing"))?;
    let program = Path::new(program);
    let registered = ["SDK_FIXTURE_RUSTC", "SDK_FIXTURE_NIGHTLY_RUSTC"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|path| {
            fs::canonicalize(path)
                .ok()
                .zip(program.canonicalize().ok())
                .is_some_and(|(a, b)| a == b)
        });
    let plain = args
        .iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>();
    // Rustc's file-name report parses crate attributes from stdin. Cache only
    // Cargo's empty source; preserve every byte for other callers.
    let input = if plain
        .as_ref()
        .is_some_and(|args| codec::pure_args(args) && args.first().is_some_and(|arg| arg == "-"))
    {
        source(&mut io::stdin().lock())?
    } else {
        None
    };
    let overridden = std::env::vars_os().any(|(key, value)| {
        let key = key.to_string_lossy();
        !value.is_empty()
            && ((key.starts_with("RUST_")
                || key.starts_with("RUSTC_")
                || key.starts_with("LD_")
                || key.starts_with("DYLD_"))
                && !matches!(key.as_ref(), "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER"))
    });
    if registered
        && !overridden
        && input.is_none()
        && plain.as_ref().is_some_and(|args| codec::pure_args(args))
    {
        if let Some(directory) = std::env::var_os("SDK_FIXTURE_COMPILER_CACHE") {
            let program = program.canonicalize()?;
            let output = cached(
                Path::new(&directory),
                &program,
                args,
                || sdk_file_identity::stamp(&program),
                || Command::new(&program).args(args).output(),
            )?;
            io::stdout().write_all(&output.stdout)?;
            io::stdout().flush()?;
            io::stderr().write_all(&output.stderr)?;
            io::stderr().flush()?;
            return Ok(output.status.code().unwrap_or(1));
        }
    }
    let mut command = Command::new(program);
    command.args(args);
    if let Some(input) = input {
        command.stdin(input);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec())
    }
    #[cfg(windows)]
    {
        Ok(command.status()?.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn compiler_stdin_preserves_every_byte_and_only_empty_input_is_reused(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            let captured = source(&mut bytes.as_slice()).unwrap();
            prop_assert_eq!(captured.is_none(), bytes.is_empty());
            if let Some(mut file) = captured {
                let mut actual = Vec::new(); file.read_to_end(&mut actual).unwrap();
                prop_assert_eq!(actual, bytes);
            }
        }
        #[test]
        fn probe_key_binds_concrete_path_identity_and_argument_boundaries(
            program in ".{0,32}", stamp in any::<u64>(), args in prop::collection::vec(".{0,16}", 0..8),
        ) {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            let before = key(Path::new(&program), &stamp, &args).unwrap();
            let other = format!("{program}!");
            prop_assert_ne!(&before, &key(Path::new(&other), &stamp, &args).unwrap());
            prop_assert_ne!(&before, &key(Path::new(&program), &stamp.wrapping_add(1), &args).unwrap());
            let mut extended = args; extended.push(OsString::new());
            prop_assert_ne!(&before, &key(Path::new(&program), &stamp, &extended).unwrap());
        }
    }
    fn mutation(broken: bool) {
        let directory = tempfile::tempdir().unwrap();
        let tool = directory.path().join("concrete-tool");
        fs::write(&tool, "A").unwrap();
        let modified = fs::metadata(&tool).unwrap().modified().unwrap();
        let query = || {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt;
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: fs::read(&tool)?,
                stderr: Vec::new(),
            })
        };
        let observe = || {
            Ok(if broken {
                Vec::new()
            } else {
                serde_json::to_vec(&sdk_file_identity::stamp(&tool)?).unwrap()
            })
        };
        assert_eq!(
            cached(directory.path(), &tool, &[], observe, query)
                .unwrap()
                .stdout,
            b"A"
        );
        assert_eq!(
            cached(directory.path(), &tool, &[], observe, || panic!(
                "unchanged compiler was queried again"
            ))
            .unwrap()
            .stdout,
            b"A"
        );
        fs::write(&tool, "B").unwrap();
        fs::File::options()
            .write(true)
            .open(&tool)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            cached(directory.path(), &tool, &[], observe, query)
                .unwrap()
                .stdout,
            b"B",
            "stale concrete probe"
        );
        assert!(codec::pure_args(&["-vV".into()]));
        assert!(run(&[]).is_err());
    }
    #[test]
    fn concrete_probe_rechecks_native_identity() {
        mutation(false);
    }
    #[test]
    #[should_panic(expected = "stale concrete probe")]
    fn negative_control_detects_omitted_compiler_identity() {
        mutation(true);
    }
}
