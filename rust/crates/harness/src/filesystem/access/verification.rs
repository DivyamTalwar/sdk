use super::*;

fn roundtrip<T: PreparationRecord + serde::Serialize + std::fmt::Debug + PartialEq>(
    io: &PreparationIo,
    bytes: &[u8],
) -> Result<()> {
    let expected = serde_json::from_slice::<T>(bytes);
    let decoded = record::<T>(io, bytes, crate::wire::ErrorCode::Storage);
    let (expected, decoded) = match (expected, decoded) {
        (Err(_), Err(_)) => return Ok(()),
        (Ok(expected), Ok(decoded)) => (expected, decoded),
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), Ok(_)) => panic!("bounded decoder accepted an invalid record: {error}"),
    };
    assert_eq!(expected, decoded.0);
    let encoded = io.encode(
        &decoded.0,
        acyclic_fs::access::MAXIMUM_POLICY_RECORD_BYTES,
        false,
    )?;
    let hold = io.guard_bytes(&encoded)?;
    let encoded = (encoded, hold);
    let repeated = record::<T>(io, &encoded.0, crate::wire::ErrorCode::Storage)?;
    assert_eq!(decoded.0, repeated.0);
    drop(repeated);
    drop(encoded);
    drop(decoded);
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| Error::Storage(error.to_string()))?;
    let repeated = io.value_decode_preparation::<T>(&value, crate::wire::ErrorCode::Storage)?;
    assert_eq!(expected, repeated.0);
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
    roundtrip::<FileRef>(&io, bytes)?;
    roundtrip::<GenerationRef>(&io, bytes)?;
    roundtrip::<(FileRef, Option<GenerationRef>)>(&io, bytes)?;
    roundtrip::<crate::Capabilities>(&io, bytes)?;
    if let Ok(capabilities) =
        record::<crate::Capabilities>(&io, bytes, crate::wire::ErrorCode::Storage)
    {
        let ceilings = crate::Capabilities::new(
            capabilities
                .0
                .iter()
                .filter(|label| label.starts_with(acyclic_fs::access::SCOPE_PREFIX)),
        );
        let legacy = crate::Capabilities::new(
            capabilities
                .0
                .iter()
                .filter(|label| !label.starts_with(acyclic_fs::access::SCOPE_PREFIX)),
        );
        if ceilings.iter().next().is_some()
            && legacy
                .iter()
                .any(acyclic_fs::access::is_filesystem_capability)
        {
            assert!(!legacy.is_subset_of(&capabilities.0));
            assert!(
                capabilities
                    .0
                    .without(&ceilings)
                    .iter()
                    .all(|label| !acyclic_fs::access::is_filesystem_capability(label))
            );
            assert!(
                capabilities
                    .0
                    .intersect(&legacy)
                    .iter()
                    .all(|label| !acyclic_fs::access::is_filesystem_capability(label))
            );
        }
    }
    assert_eq!(io.owned_bytes()?, 0);
    Ok(())
}
