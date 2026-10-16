use crate::{io_error, CliError};
use sdk_file_identity::{stamp, Stamp};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

type Values<S> = RefCell<BTreeMap<(PathBuf, String), (S, String)>>;
thread_local! {
    // Calling-thread ownership; every public command starts with empty state.
    static VALUES: Values<Stamp> = const { RefCell::new(BTreeMap::new()) };
    static CONCRETE: RefCell<BTreeSet<PathBuf>> = const { RefCell::new(BTreeSet::new()) };
    // Only complete, closed owner inventories reach this cache. Both concrete
    // executables and every source/config/environment input remain in its key.
    static METADATA: Values<(Stamp, Stamp)> = const { RefCell::new(BTreeMap::new()) };
}

fn clear<S>(values: &Values<S>) {
    values.borrow_mut().clear();
}

pub(super) fn begin_run() {
    VALUES.with(clear);
    CONCRETE.with_borrow_mut(BTreeSet::clear);
}

pub(super) fn resolved_cargo(path: &Path) {
    CONCRETE.with_borrow_mut(|paths| paths.insert(path.to_owned()));
}

pub(super) fn is_resolved_cargo(path: &Path) -> bool {
    CONCRETE.with_borrow(|paths| paths.contains(path))
}

fn observed<S: Clone + Eq>(
    values: &Values<S>,
    key: (PathBuf, String),
    observe: impl FnMut() -> Result<S, CliError>,
    initialize: impl FnOnce() -> Result<String, CliError>,
) -> Result<String, CliError> {
    sdk_file_identity::cached(values, key, observe, initialize, || {
        CliError("file changed while reading identity".into())
    })
}

pub(super) fn get(
    path: &Path,
    context: &str,
    initialize: impl FnOnce() -> Result<String, CliError>,
) -> Result<String, CliError> {
    let key = (
        std::path::absolute(path).map_err(io_error)?,
        context.to_owned(),
    );
    VALUES.with(|values| observed(values, key, || stamp(path).map_err(io_error), initialize))
}

pub(super) fn sha256_file(path: &Path) -> Result<String, CliError> {
    get(path, "sha256", || {
        let bytes = sdk_docs::read_rustdoc(path).map_err(crate::docs_error)?;
        Ok(crate::sha256_bytes(&bytes))
    })
}

pub(super) fn owner_metadata(
    cargo: &Path,
    context: &str,
    query: impl FnOnce() -> Result<String, CliError>,
) -> Result<String, CliError> {
    let compiler = cargo
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    let compiler = compiler.canonicalize().ok();
    let configured =
        crate::command_var_os("RUSTC").and_then(|path| PathBuf::from(path).canonicalize().ok());
    if !is_resolved_cargo(cargo)
        || compiler.is_none()
        || configured.is_none()
        || configured != compiler
        || ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"]
            .iter()
            .any(|key| crate::command_var_os(key).is_some_and(|value| !value.is_empty()))
    {
        return get(cargo, context, query);
    }
    let compiler = compiler.unwrap();
    METADATA.with(|values| {
        observed(
            values,
            (cargo.to_owned(), context.to_owned()),
            || {
                Ok((
                    stamp(cargo).map_err(io_error)?,
                    stamp(&compiler).map_err(io_error)?,
                ))
            },
            query,
        )
    })
}

pub(super) fn tool_identity(path: &Path) -> Result<String, CliError> {
    let cache = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .join("target/sdk-tool-identities-v2");
    get(path, "tool-sha256", || shared_identity(path, &cache))
}

fn shared_identity(path: &Path, cache: &Path) -> Result<String, CliError> {
    let path = std::path::absolute(path).map_err(io_error)?;
    let before = stamp(&path).map_err(io_error)?;
    let path_key = crate::sha256_bytes(path.as_os_str().as_encoded_bytes());
    let Ok(bytes) = serde_json::to_vec(&(path.as_os_str().as_encoded_bytes(), &before)) else {
        return crate::sha256_file(&path);
    };
    let key = crate::sha256_bytes(&bytes);
    let record = cache.join(path_key.trim_start_matches("sha256:"));
    if fs::create_dir_all(cache).is_err() {
        return crate::sha256_file(&path);
    }
    let lock = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join(format!("{}.lock", path_key.trim_start_matches("sha256:"))))
    {
        Ok(lock) => lock,
        Err(_) => return crate::sha256_file(&path),
    };
    let unchanged = || {
        if stamp(&path).map_err(io_error)? != before {
            return Err(CliError("file changed while reading identity".into()));
        }
        Ok(())
    };
    crate::shared_cache::get_or_compute(
        &lock,
        || {
            unchanged().map_err(std::io::Error::other)?;
            use std::io::Read;
            let cached = fs::File::open(&record)
                .ok()
                .and_then(|file| {
                    let mut bytes = Vec::new();
                    file.take(513).read_to_end(&mut bytes).ok().map(|_| bytes)
                })
                .and_then(|bytes| crate::cache_record::decode(&bytes, &key));
            unchanged().map_err(std::io::Error::other)?;
            Ok(cached)
        },
        || {
            let digest = crate::sha256_file(&path).map_err(std::io::Error::other)?;
            unchanged().map_err(std::io::Error::other)?;
            Ok(digest)
        },
        |digest| {
            use std::io::Write;
            let mut file = tempfile::NamedTempFile::new_in(cache)?;
            file.write_all(&crate::cache_record::encode(&key, digest))?;
            file.persist(&record).map_err(|error| error.error)?;
            Ok(())
        },
    )
    .map_err(io_error)
}

pub(super) fn rustdoc_digest(path: &Path, root: &Path) -> Result<String, CliError> {
    let context = format!(
        "rustdoc:{}",
        crate::sha256_bytes(root.as_os_str().as_encoded_bytes())
    );
    get(path, &context, || {
        sdk_docs::rustdoc_digest(path, root).map_err(|error| CliError(error.to_string()))
    })
}

#[cfg(test)]
mod tests;
