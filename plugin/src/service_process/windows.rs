//! Same-user Windows named-pipe connections.

use super::*;

/// Connects to the service's pipe, trying `attempts` times while none
/// exists. A pipe whose every instance is connected is busy only until the
/// service creates the next, so that wait counts no attempt.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "waiting for a busy pipe needs the WaitNamedPipeW Win32 call"
)]
pub(crate) async fn connect_windows_control_pipe(
    data: &Path,
    attempts: usize,
    deadline: tokio::time::Instant,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ControlRequestError> {
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    let pipe = windows_control_pipe_path(data);
    let mut attempt = 1;
    loop {
        let remaining = remaining_control_wait(deadline)?;
        let error = match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe) {
            // Only this user's own service may answer.
            Ok(client) => {
                return match acyclic_native_runtime::UserPipeSecurity::current_user()
                    .and_then(|security| security.authenticate(&client))
                {
                    Ok(()) => Ok(client),
                    Err(error) => Err(ControlRequestError::Unavailable(format!(
                        "cannot identify the Acyclic service pipe's server: {error}"
                    ))),
                };
            }
            Err(error) => error,
        };
        if error.raw_os_error() == i32::try_from(ERROR_PIPE_BUSY).ok() {
            let name = pipe.encode_utf16().chain([0]).collect::<Vec<_>>();
            let wait = u32::try_from(remaining.as_millis())
                .unwrap_or(u32::MAX)
                .max(1);
            // A failed wait (the pipe vanished) shows in the next open.
            // SAFETY: `name` is a live NUL-terminated UTF-16 string that the
            // task owns for the whole call.
            let _ =
                tokio::task::spawn_blocking(move || unsafe { WaitNamedPipeW(name.as_ptr(), wait) })
                    .await;
            continue;
        }
        if attempt >= attempts {
            return Err(ControlRequestError::Unavailable(format!(
                "Acyclic service is not running: {error}"
            )));
        }
        attempt += 1;
        tokio::time::sleep(std::time::Duration::from_millis(20).min(remaining)).await;
    }
}

#[cfg(windows)]
pub(crate) fn windows_control_pipe_path(data: &Path) -> String {
    format!(
        r"\\.\pipe\acyclic-{}",
        short_hash(data.as_os_str().to_string_lossy().as_bytes())
    )
}
