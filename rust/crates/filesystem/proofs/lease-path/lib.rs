#![deny(unsafe_code)]

#[path = "../../src/operation_window/lease_path/encoding.rs"]
mod encoding;

#[cfg(kani)]
mod proofs {
    use super::encoding;

    #[kani::proof]
    #[kani::unwind(21)]
    fn every_lease_path_fits_its_exact_buffer() {
        let expiry: u64 = kani::any();
        let workspace: [u8; 16] = kani::any();
        let lease: [u8; 16] = kani::any();
        let mut text = [0; encoding::MAX_ENCODED_BYTES];
        let length = encoding::length(expiry);
        encoding::write(workspace, lease, expiry, length, &mut text);
        assert!(length <= encoding::MAX_ENCODED_BYTES);
        assert!(length >= encoding::MAX_ENCODED_BYTES - 19);
    }
}
