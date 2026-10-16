pub(super) const PREFIX: &[u8] = b"fs/operation-windows-v1/";
pub(super) const SEPARATOR: &[u8] = b"/leases/";
pub(crate) const MAX_ENCODED_BYTES: usize = PREFIX.len() + 32 + SEPARATOR.len() + 32 + 1 + 20;

pub(super) fn length(expires_at_millis: u64) -> usize {
    let digits = expires_at_millis
        .checked_ilog10()
        .map_or(1, |log| log as usize + 1);
    PREFIX.len() + 32 + SEPARATOR.len() + 32 + 1 + digits
}

// `length` is obtained from the expiry before the caller admits any writes.
pub(super) fn write(
    workspace: [u8; 16],
    lease: [u8; 16],
    expires_at_millis: u64,
    length: usize,
    text: &mut [u8; MAX_ENCODED_BYTES],
) {
    text[..PREFIX.len()].copy_from_slice(PREFIX);
    let mut cursor = PREFIX.len();
    hex::encode_to_slice(workspace, &mut text[cursor..cursor + workspace.len() * 2])
        .expect("fixed-width workspace lease path has its exact hex capacity");
    cursor += workspace.len() * 2;
    text[cursor..cursor + SEPARATOR.len()].copy_from_slice(SEPARATOR);
    cursor += SEPARATOR.len();
    hex::encode_to_slice(lease, &mut text[cursor..cursor + lease.len() * 2])
        .expect("fixed-width lease identity has its exact hex capacity");
    cursor += lease.len() * 2;
    text[cursor] = b'/';
    let mut number = expires_at_millis;
    for position in (cursor + 1..length).rev() {
        text[position] = b'0' + (number % 10) as u8;
        number /= 10;
    }
}
