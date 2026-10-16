use acyclic_harness::{
    client::{CommandResolution, FileMention},
    conversation::{FileDescriptor, FileRef, VolumeRef},
    model::{FileProjectionPolicy, ModelContent, ModelContentPart},
};

fn file(id: u8, revision: u8, volume: u8, body: &[u8]) -> FileRef {
    let fixture: FileRef =
        serde_json::from_str(include_str!("../fixtures/v2/file-ref.json")).unwrap();
    let reference = fixture.volume();
    FileRef::new(
        VolumeRef::new(
            reference.provider().clone(),
            format!("volume-{volume}"),
            reference.class(),
            reference.owner().clone(),
            &mut None,
        )
        .unwrap(),
        format!("file-{id}"),
        format!("revision-{revision}"),
        FileDescriptor::from_bytes(body, "text/plain", &mut None).unwrap(),
        "file".into(),
        &mut None,
    )
    .unwrap()
}

#[test]
fn conflicting_metadata_for_one_pinned_identity_is_rejected() {
    let mentions = [b"first".as_slice(), b"second".as_slice()]
        .into_iter()
        .enumerate()
        .map(|(index, body)| FileMention {
            start: index * 8,
            end: index * 8 + 7,
            file: file(1, 1, 1, body),
        })
        .collect();
    assert!(matches!(
        CommandResolution::Prompt {
            text: "@file-1 @file-1".into(),
            source: None,
            mentions,
        }
        .into_model_input(),
        Err(acyclic_harness::Error::Conflict(_))
    ));
}

proptest::proptest! {
    // Property: attach each pinned (volume,path,version) exactly once, in first
    // mention order; preserve prompt text and the ordinary model-input codec.
    #[test]
    fn attachment_projection_preserves_pinned_identity_order_and_roundtrips(
        identities in proptest::collection::vec((0u8..8, 0u8..4, 0u8..4), 0..64),
        prefix in "[a-zA-Z ]{1,32}",
    ) {
        let mut text = prefix;
        let mut mentions = Vec::new();
        let mut expected = Vec::new();
        for (id, revision, volume) in identities {
            let path = format!("file-{id}");
            let identity = (format!("volume-{volume}"), path.clone(), format!("revision-{revision}"));
            text.push(' ');
            let start = text.len();
            text.push('@');
            text.push_str(&path);
            mentions.push(FileMention { start, end: text.len(), file: file(id, revision, volume, b"body") });
            if !expected.contains(&identity) {
                expected.push(identity);
            }
        }
        let original = text.clone();
        let content = CommandResolution::Prompt { text, source: None, mentions }.into_model_input().unwrap();
        match &content {
            ModelContent::Text(text) => {
                proptest::prop_assert!(expected.is_empty());
                proptest::prop_assert_eq!(text, &original);
            }
            ModelContent::Parts(parts) => {
                proptest::prop_assert_eq!(parts.len(), expected.len() + 1);
                proptest::prop_assert_eq!(&parts[0], &ModelContentPart::Text { text: original });
                for (part, (volume, path, version)) in parts[1..].iter().zip(expected) {
                    match part {
                        ModelContentPart::File { file, policy } => {
                            proptest::prop_assert_eq!(file.volume().id(), volume);
                            proptest::prop_assert_eq!(file.path(), path);
                            proptest::prop_assert_eq!(file.version(), version);
                            proptest::prop_assert_eq!(policy, &FileProjectionPolicy::BoundedFull);
                        }
                        _ => proptest::prop_assert!(false, "attachment must be a file"),
                    }
                }
            }
            _ => proptest::prop_assert!(false, "complete prompt required"),
        }
        let restored: ModelContent = serde_json::from_slice(&serde_json::to_vec(&content).unwrap()).unwrap();
        proptest::prop_assert_eq!(restored, content);
    }
}
