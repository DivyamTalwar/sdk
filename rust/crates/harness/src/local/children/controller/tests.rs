use super::*;
use proptest::prelude::*;

fn file(reference: &Component, kind: ChildFile, body: Vec<u8>) -> FileValue {
    FileValue::new(
        FileKey::Child {
            reference: reference.clone(),
            file: kind,
        },
        body,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    // Exhaust the callback before, between, and after the two ordered follow
    // records. Only the follow may update cached files; completion releases its
    // worker only after the accepted status cursor has been observed.
    #[test]
    fn completed_jobs_never_hide_new_inputs(prefix in 1u64..9, suffix in 1u64..9) {
        let reference = Component::new("worker").unwrap();
        let task = TaskId::from_bytes([8;16]);
        for callback_at in 0..3 {
            let mut values = BTreeMap::new();
            let request = file(&reference, ChildFile::Request, b"work".to_vec());
            values.insert(request.key.clone(), request);
            for sequence in 1..prefix {
                let value = file(&reference, ChildFile::Inbox(Component::new(format!("message-{sequence}")).unwrap()), vec![1]);
                values.insert(value.key.clone(), value);
            }
            let status = ChildStatus { task, state: ChildState::Running, settled: false, inputs: prefix };
            let started = file(&reference, ChildFile::Status, serde_json::to_vec(&status).unwrap());
            values.insert(started.key.clone(), started);
            let mut active = BTreeSet::from([reference.clone()]);
            let mut completions = BTreeMap::new();
            let mut position = 9;
            let mut scheduled = 0;
            let mut followed = 0;
            for step in 0..3 {
                let input_changed = if step == callback_at {
                    completions.insert(reference.clone(), 10);
                    false
                } else {
                    followed += 1;
                    position += 1;
                    if followed == 1 {
                        let done = file(&reference, ChildFile::Status, serde_json::to_vec(&ChildStatus { settled: true, ..status }).unwrap());
                        values.insert(done.key.clone(), done);
                        false
                    } else {
                        for sequence in prefix..prefix+suffix {
                            let value = file(&reference, ChildFile::Inbox(Component::new(format!("message-{sequence}")).unwrap()), vec![2]);
                            values.insert(value.key.clone(), value);
                        }
                        true
                    }
                };
                let ready = release_completed(position, &mut active, &mut completions);
                if (input_changed || ready.contains(&reference))
                    && !active.contains(&reference) && needs_work(&reference, &values).unwrap() {
                    scheduled += 1;
                    active.insert(reference.clone());
                }
            }
            prop_assert!(completions.is_empty());
            prop_assert_eq!(scheduled, 1);
            prop_assert_eq!(input_count(&reference, &values), prefix + suffix);
        }
    }
}
