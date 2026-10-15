use super::*;

#[cfg(not(target_os = "linux"))]
pub(crate) async fn handle_control_connection<S>(
    stream: S,
    control: Arc<impl ConcurrentControlRequestDispatcher + 'static>,
    ledger: Arc<ControlLedger>,
    mut shutdown: watch::Receiver<bool>,
) -> acyclic_harness::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut original_native_work = None;
    let mut stream = stream;
    let mut request = Vec::new();
    let read = async {
        let reader = BufReader::new(&mut stream);
        let mut bounded = reader.take((MAXIMUM_CONTROL_MESSAGE_BYTES + 1) as u64);
        bounded
            .read_until(b'\n', &mut request)
            .await
            .map_err(display)
    };
    tokio::select! {
        read = read => {
            read.map_err(acyclic_harness::Error::Storage)?;
        }
        changed = shutdown.changed() => {
            let _ = changed;
            return Ok(());
        }
    }
    let dispatch = async {
        if request.len() > MAXIMUM_CONTROL_MESSAGE_BYTES {
            uncorrelated_control_response("Acyclic control request exceeds the 4 MiB bound")
        } else if request.last() != Some(&b'\n') {
            uncorrelated_control_response("Acyclic control request must end with a newline")
        } else {
            request.pop();
            match serde_json::from_slice::<ControlEnvelope<ControlRequest>>(&request) {
                Ok(envelope) => {
                    dispatch_control_envelope(
                        &control,
                        &ledger,
                        envelope,
                        &mut original_native_work,
                    )
                    .await
                }
                Err(error) => invalid_control_request_response(&request, &error),
            }
        }
    };
    tokio::pin!(dispatch);
    let mut peer_byte = [0_u8; 1];
    let encoded = tokio::select! {
        response = &mut dispatch => response,
        peer = stream.read(&mut peer_byte) => {
            match peer {
                Ok(0) => {
                    let encoded = dispatch.await?;
                    let failure = encoded.2;
                    let allocation = encoded.1;
                    let response = encoded.0;
                    drop(stream);
                    drop(response);
                    drop(allocation);
                    drop(failure);
                    return Ok(());
                }
                Ok(_) => return Err(acyclic_harness::Error::Invalid(
                    "Acyclic control request included trailing bytes".to_owned(),
                )),
                Err(error) if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                ) => {
                    let encoded = dispatch.await?;
                    let failure = encoded.2;
                    let allocation = encoded.1;
                    let response = encoded.0;
                    drop(stream);
                    drop(response);
                    drop(allocation);
                    drop(failure);
                    return Ok(());
                }
                Err(error) => return Err(acyclic_harness::Error::Storage(display(error))),
            }
        }
    }?;
    let failure = encoded.2;
    let allocation = encoded.1;
    let response = encoded.0;
    // Cancellation closes the peer before releasing response bytes or native custody.
    let mut stream = stream;
    let write = async {
        stream.write_all(&response).await.map_err(display)?;
        stream.write_all(b"\n").await.map_err(display)?;
        stream.flush().await.map_err(display)
    };
    let delivered = deliver_control_response(write, shutdown).await;
    drop(stream);
    drop(response);
    drop(allocation);
    drop(failure);
    delivered.map_err(acyclic_harness::Error::Storage)
}
