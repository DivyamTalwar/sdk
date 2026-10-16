#![no_main]

#[path = "../../../crates/filesystem/src/native_mount/projfs/tests/metadata_output.rs"]
mod output;

use std::future::Future;
use std::io;
use std::task::{Context, Poll, Waker};

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let mut reader = bytes;
    let mut line = String::new();
    let go = std::cell::Cell::new(false);
    let actual = {
        let mut future = std::pin::pin!(output::after_ready(
            async {
                if output::read_output(&mut reader, &mut line, true, |_| {}).await? {
                    Ok(())
                } else {
                    Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                }
            },
            || Ok::<_, io::Error>(()),
            || {
                go.set(true);
                Ok(())
            },
        ));
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    };
    let mut expected = Err(io::ErrorKind::UnexpectedEof);
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        match std::str::from_utf8(line) {
            Ok(line) if line.trim_end() == output::READY => {
                expected = Ok(true);
                break;
            }
            Ok(_) => {}
            Err(_) => {
                expected = Err(io::ErrorKind::InvalidData);
                break;
            }
        }
    }
    match actual {
        Poll::Ready(result) => {
            assert_eq!(
                result.map(|()| go.get()).map_err(|error| error.kind()),
                expected
            );
            assert_eq!(go.get(), expected.is_ok());
        }
        Poll::Pending => panic!("in-memory output cannot block"),
    }
});
