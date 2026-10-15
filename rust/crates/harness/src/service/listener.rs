use super::{ServiceEndpoint, frames, storage};
use crate::Result;

#[cfg(unix)]
type SocketListener = tokio::net::UnixListener;
#[cfg(windows)]
type SocketListener = tokio::net::windows::named_pipe::NamedPipeServer;

pub(super) struct Listener {
    #[cfg(windows)]
    endpoint: ServiceEndpoint,
    socket: SocketListener,
}

impl Listener {
    pub fn bind(endpoint: ServiceEndpoint) -> Result<Self> {
        #[cfg(unix)]
        let socket = {
            let path = endpoint.directory.join("harness.sock");
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
            }
            tokio::net::UnixListener::bind(path).map_err(storage)?
        };
        #[cfg(windows)]
        let socket = match endpoint.security.create(&endpoint.pipe, true) {
            Ok(socket) => socket,
            // IOCP may still own a closing instance after its service releases the
            // process lock. Reuse only an object whose owner and protected DACL
            // the native boundary authenticates as this user.
            Err(error) if error.raw_os_error() == Some(5) => endpoint
                .security
                .create(&endpoint.pipe, false)
                .map_err(storage)?,
            Err(error) => return Err(storage(error)),
        };
        Ok(Self {
            #[cfg(windows)]
            endpoint,
            socket,
        })
    }

    pub async fn accept(&mut self) -> Result<frames::Connection> {
        let listener = &mut self.socket;
        #[cfg(unix)]
        {
            listener
                .accept()
                .await
                .map(|(socket, _)| frames::connection(socket))
                .map_err(storage)
        }
        #[cfg(windows)]
        {
            listener.connect().await.map_err(storage)?;
            let next = self
                .endpoint
                .security
                .create(&self.endpoint.pipe, false)
                .map_err(storage)?;
            Ok(frames::connection(std::mem::replace(listener, next)))
        }
    }
}
