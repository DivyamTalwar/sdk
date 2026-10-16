use crate::{io_error, CliError};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    native: (u64, u64, i64, i64),
    #[cfg(windows)]
    native: (u64, [u8; 16], i64),
}

fn stamp(path: &Path) -> Result<Stamp, CliError> {
    #[cfg(windows)]
    let file = fs::File::open(path).map_err(io_error)?;
    #[cfg(windows)]
    let metadata = file.metadata().map_err(io_error)?;
    #[cfg(not(windows))]
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() {
        return Err(CliError(format!(
            "expected a regular file: {}",
            path.display()
        )));
    }
    Ok(Stamp {
        length: metadata.len(),
        modified: metadata.modified().map_err(io_error)?,
        created: metadata.created().ok(),
        #[cfg(unix)]
        native: {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        },
        #[cfg(windows)]
        native: windows_identity(&file).map_err(io_error)?,
    })
}

#[cfg(windows)]
fn windows_identity(file: &fs::File) -> std::io::Result<(u64, [u8; 16], i64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };
    let mut basic = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
    let mut id = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: both buffers have the API's exact type/size and outlive the calls;
    // the file owns the live handle. Read the buffers only after both succeed.
    unsafe {
        if GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        ) == 0
            || GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                id.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let id = id.assume_init();
        Ok((
            id.VolumeSerialNumber,
            id.FileId.Identifier,
            basic.assume_init().ChangeTime,
        ))
    }
}

thread_local! {
    // Calling-thread ownership avoids locks and cross-invocation environment races.
    static VALUES: RefCell<BTreeMap<(PathBuf, String), (Stamp, String)>> = const { RefCell::new(BTreeMap::new()) };
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
    let before = stamp(&key.0)?;
    if let Some(value) = VALUES.with(|values| {
        values
            .borrow()
            .get(&key)
            .filter(|(cached, _)| *cached == before)
            .map(|(_, value)| value.clone())
    }) {
        return Ok(value);
    }
    let value = initialize()?;
    if stamp(&key.0)? != before {
        return Err(CliError(format!(
            "file changed while reading identity: {}",
            path.display()
        )));
    }
    VALUES.with(|values| values.borrow_mut().insert(key, (before, value.clone())));
    Ok(value)
}

pub(super) fn sha256_file(path: &Path) -> Result<String, CliError> {
    get(path, "sha256", || crate::sha256_file(path))
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
