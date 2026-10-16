use super::*;
use acyclic_stream::{MemoryStream, StreamPath, verification::block_on};

fn first_publication(broken: bool) {
    loom::model(move || {
        // A single competing create has exactly three cuts: before our read,
        // after that read/before our CAS, or after our CAS. TaskVolume's create
        // models cover the atomic commit itself; exhaust these caller cuts
        // using the production retry loop and instrumented volume locks.
        for cut in 0..3 {
            let volume = block_on(TaskVolume::open(
                Arc::new(MemoryStream::default()),
                StreamPath::new("inheritance-model").unwrap(),
            ))
            .unwrap();
            let key =
                FileKey::parse(".tasks/child/context/command-inherited/manifest.json").unwrap();
            if cut == 0 {
                block_on(retain(&volume, key.clone(), json!(false), 1024)).unwrap();
            }
            let snapshot = block_on(volume.snapshot(&Query::File(key.clone()), 1)).unwrap();
            if cut == 1 {
                block_on(retain(&volume, key.clone(), json!(false), 1024)).unwrap();
            }
            let accepted = block_on(retain_at_cut(
                &volume,
                key.clone(),
                json!(true),
                1024,
                snapshot,
            ))
            .unwrap();
            if cut == 2 {
                block_on(retain(&volume, key.clone(), json!(false), 1024)).unwrap();
            }
            let returned = if broken { json!(true) } else { accepted };
            let saved = block_on(read(&volume, &key, 1024)).unwrap().unwrap();
            assert_eq!(
                returned, saved,
                "competing creator returned unretained inheritance"
            );
        }
    });
}
#[test]
fn concurrent_inheritance_creation_returns_the_first_retained_binding() {
    first_publication(false);
}
#[test]
#[should_panic(expected = "competing creator returned unretained inheritance")]
fn negative_control_detects_using_the_losing_inheritance() {
    first_publication(true);
}

#[test]
fn partial_inheritance_publication_completes_without_replacing_saved_fields() {
    loom::model(|| {
        for cut in 0..3 {
            let volume = block_on(TaskVolume::open(
                Arc::new(MemoryStream::default()),
                StreamPath::new("partial-inheritance").unwrap(),
            ))
            .unwrap();
            let task = TaskId::from_bytes([1; 16]);
            let mut expected = json!({"model":"new model","summary":"new summary","parent":null});
            for field in ["model", "summary"].into_iter().take(cut) {
                let value = json!(format!("saved {field}"));
                block_on(retain(
                    &volume,
                    key(task, &format!("inherited-{field}")).unwrap(),
                    value.clone(),
                    128,
                ))
                .unwrap();
                expected[field] = value;
            }
            assert_eq!(
                block_on(read_inheritance(&volume, task, 128)).unwrap(),
                None
            );
            let accepted = block_on(retain_inheritance(
                &volume,
                task,
                json!({"model":"new model","summary":"new summary","parent":null}),
                128,
            ))
            .unwrap();
            assert_eq!(accepted, expected);
            assert_eq!(
                block_on(read_inheritance(&volume, task, 128)).unwrap(),
                Some(expected)
            );
        }
    });
}
