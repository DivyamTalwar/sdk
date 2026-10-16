use super::*;
use proptest::prelude::*;

proptest! {
    // Every u64 locator round-trips; every other width is rejected.
    #[test]
    fn origin_sequence_has_one_exact_width(sequence in any::<u64>(), bytes in prop::collection::vec(any::<u8>(), 0..24)) {
        prop_assert_eq!(origin_sequence::decode(&sequence.to_le_bytes()), Some(sequence));
        prop_assert_eq!(origin_sequence::decode(&bytes).is_some(), bytes.len() == 8);
    }
}

#[tokio::test]
async fn a_separately_written_origin_locator_cannot_authenticate_a_source() -> Result<()> {
    let client = Arc::new(MemoryStream::default());
    let operation = OperationId::from_bytes([132; 16]);
    let event = SchedulerEvent::Declared {
        spec: Box::new(spec(operation, 0)?),
    };
    let canonical = crate::contract::canonical_json_bytes(&event)?;
    let encoded = encode(
        1,
        operation,
        "seed",
        *blake3::hash(&canonical).as_bytes(),
        canonical,
        1,
        &mut None,
    )?;
    Stream::new(Arc::clone(&client), COORDINATOR_PATH)?
        .append_batch(vec![encoded.into()], Some(0), None)
        .await?;
    let path = origins::event_path(&event, &mut None)?.unwrap();
    Stream::new(Arc::clone(&client), path.as_str())?
        .append_batch(vec![0u64.to_le_bytes().to_vec().into()], Some(0), None)
        .await?;
    let error = origins::read(&client, path, None, &mut None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), wire::ErrorCode::Storage);
    assert!(error.to_string().contains("not atomic"));
    Ok(())
}
