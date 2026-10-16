use super::*;

fn roundtrip<T: PreparationRecord + serde::Serialize + std::fmt::Debug + PartialEq>(
    io: &PreparationIo,
    bytes: &[u8],
) -> Result<()> {
    let Ok(decoded) = record::<T>(io, bytes, crate::wire::ErrorCode::Storage) else {
        return Ok(());
    };
    let encoded = io.encode(
        &decoded.0,
        acyclic_fs::access::MAXIMUM_POLICY_RECORD_BYTES,
        false,
    )?;
    let hold = io.guard_bytes(&encoded)?;
    let encoded = (encoded, hold);
    let repeated = record::<T>(io, &encoded.0, crate::wire::ErrorCode::Storage)?;
    assert_eq!(decoded.0, repeated.0);
    Ok(())
}

/// Shared bounded-decoder oracle for cargo-fuzz, seeds and shrinking properties.
pub fn check_access_record(bytes: &[u8]) -> Result<()> {
    if bytes.len() > 4096 {
        return Ok(());
    }
    let io = PreparationIo::from_budget(
        acyclic_fs::performance::BOUNDARY_BUDGET,
        &acyclic_fs::CancellationToken::new(),
    )?;
    roundtrip::<Policy>(&io, bytes)?;
    roundtrip::<Capability>(&io, bytes)?;
    assert_eq!(io.owned_bytes()?, 0);
    Ok(())
}
