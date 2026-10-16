use crate::{public_api::PublicItemSignature, Error};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Default)]
struct Cache {
    rendered: RefCell<BTreeMap<Vec<u8>, Vec<PublicItemSignature>>>,
    files: RefCell<BTreeMap<PathBuf, (sdk_file_identity::Stamp, Rc<[u8]>)>>,
    parsed: RefCell<BTreeMap<Rc<[u8]>, Rc<rustdoc_types::Crate>>>,
    json: RefCell<BTreeMap<Rc<[u8]>, Rc<serde_json::Value>>>,
}
thread_local! {
    // Every entry belongs to one calling operation; nested operations restore it.
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
    #[cfg(test)]
    static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Reuse immutable Rustdoc reads, decoding and rendering within one operation.
pub fn with_rustdoc_cache<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Cache>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CACHE.with_borrow_mut(|cache| *cache = self.0.take());
        }
    }
    let _restore = Restore(CACHE.replace(Some(Cache::default())));
    operation()
}

/// Read a Rustdoc receipt, reusing only matching native file observations.
pub fn read_rustdoc(path: &Path) -> Result<Rc<[u8]>, Error> {
    let read = || {
        std::fs::read(path)
            .map(|bytes| {
                #[cfg(test)]
                READS.set(READS.get() + 1);
                Rc::from(bytes)
            })
            .map_err(Error::from)
    };
    CACHE.with_borrow(|cache| match cache {
        Some(cache) => sdk_file_identity::cached(
            &cache.files,
            path.to_owned(),
            || sdk_file_identity::stamp(path).map_err(Error::from),
            read,
            || {
                Error::Invalid(format!(
                    "Rustdoc receipt changed while reading {}",
                    path.display()
                ))
            },
        ),
        None => read(),
    })
}

/// Read the unchanged JSON tree used for source retention and normalization.
pub fn read_rustdoc_json(path: &Path) -> Result<Rc<serde_json::Value>, Error> {
    Ok(json(&read_rustdoc(path)?)?)
}

pub(crate) fn json(raw: &[u8]) -> Result<Rc<serde_json::Value>, serde_json::Error> {
    if let Some(value) = CACHE.with_borrow(|cache| {
        cache
            .as_ref()
            .and_then(|cache| cache.json.borrow().get(raw).cloned())
    }) {
        return Ok(value);
    }
    let value = Rc::new(serde_json::from_slice(raw)?);
    CACHE.with_borrow(|cache| {
        if let Some(cache) = cache {
            cache
                .json
                .borrow_mut()
                .insert(Rc::from(raw), Rc::clone(&value));
        }
    });
    Ok(value)
}

pub(crate) fn decode(raw: Rc<[u8]>) -> Result<Rc<rustdoc_types::Crate>, serde_json::Error> {
    if let Some(parsed) = CACHE.with_borrow(|cache| {
        cache
            .as_ref()
            .and_then(|cache| cache.parsed.borrow().get(&raw).cloned())
    }) {
        return Ok(parsed);
    }
    let parsed = Rc::new(crate::public_api::decode::from_slice(&raw)?);
    CACHE.with_borrow(|cache| {
        if let Some(cache) = cache {
            cache.parsed.borrow_mut().insert(raw, Rc::clone(&parsed));
        }
    });
    Ok(parsed)
}

pub(crate) fn extract(
    bytes: &[u8],
    render: impl FnOnce() -> Result<Vec<PublicItemSignature>, Error>,
) -> Result<Vec<PublicItemSignature>, Error> {
    if let Some(items) = CACHE.with_borrow(|cache| {
        cache
            .as_ref()
            .and_then(|cache| cache.rendered.borrow().get(bytes).cloned())
    }) {
        return Ok(items);
    }
    let items = render()?;
    CACHE.with_borrow(|cache| {
        if let Some(cache) = cache {
            cache
                .rendered
                .borrow_mut()
                .insert(bytes.to_vec(), items.clone());
        }
    });
    Ok(items)
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rustdoc_types::Id;

    fn item(bytes: &[u8]) -> Vec<PublicItemSignature> {
        vec![PublicItemSignature {
            id: Id(0),
            parent_id: None,
            display: format!("{bytes:?}"),
            path: Vec::new(),
        }]
    }

    proptest! {
        #[test]
        fn json_reuse_preserves_all_fields_and_round_trips(values in prop::collection::btree_map(".{0,12}", ".{0,24}", 0..12)) {
            with_rustdoc_cache(|| {
                let bytes = serde_json::to_vec(&values).unwrap();
                let first = json(&bytes).unwrap();
                prop_assert!(Rc::ptr_eq(&first, &json(&bytes).unwrap()));
                prop_assert_eq!(&*first, &serde_json::to_value(&values).unwrap());
                prop_assert_eq!(&*json(&serde_json::to_vec(&*first).unwrap()).unwrap(), &*first);
                let changed = serde_json::to_vec(&(values, "different JSON shape")).unwrap();
                prop_assert_ne!(&*first, &*json(&changed).unwrap());
                prop_assert!(json(b"truncated {").is_err(), "truncated JSON must fail");
                Ok(())
            })?;
        }
        #[test]
        fn decoded_receipts_bind_every_input_byte(version in ".{0,20}") {
            let mut value: serde_json::Value = serde_json::from_slice(include_bytes!("../fuzz/corpus/rustdoc_adapter/valid.json")).unwrap();
            with_rustdoc_cache(|| {
                for version in [version.clone(), format!("{version}!")] {
                    value["crate_version"] = serde_json::json!(&version);
                    let bytes: Rc<[u8]> = serde_json::to_vec(&value).unwrap().into();
                    let first = decode(Rc::clone(&bytes)).unwrap();
                    let second = decode(bytes).unwrap();
                    prop_assert!(Rc::ptr_eq(&first, &second));
                    prop_assert_eq!(first.crate_version.as_deref(), Some(version.as_str()));
                }
                Ok(())
            })?;
        }
        #[test]
        fn reuse_requires_every_input_byte_and_ends_with_the_operation(
            inputs in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..32), 0..32),
            tag in any::<u8>(),
        ) {
            let mut calls = 0;
            with_rustdoc_cache(|| {
                // A prefix-only cache key aliases these two distinct inputs.
                let mut inputs = inputs;
                inputs.extend([vec![tag, 0], vec![tag, 1]]);
                let unique = inputs.iter().collect::<std::collections::BTreeSet<_>>().len();
                for bytes in inputs.iter().chain(&inputs) {
                    let actual = extract(bytes, || { calls += 1; Ok(item(bytes)) }).unwrap();
                    prop_assert_eq!(actual, item(bytes));
                }
                prop_assert_eq!(calls, unique);
                Ok(())
            })?;
            prop_assert!(CACHE.with_borrow(|cache| cache.is_none()));
        }
    }

    #[test]
    fn receipt_reads_reuse_unchanged_bytes_and_recheck_native_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt");
        std::fs::write(&path, br#"{"version":1}"#).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        with_rustdoc_cache(|| {
            let before = READS.get();
            let first = read_rustdoc(&path).unwrap();
            let second = read_rustdoc(&path).unwrap();
            assert!(Rc::ptr_eq(&first, &second));
            let original_json = read_rustdoc_json(&path).unwrap();
            assert_eq!(original_json["version"], 1);
            assert_eq!(READS.get() - before, 1);
            std::fs::write(&path, br#"{"version":2}"#).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
            assert_eq!(&*read_rustdoc(&path).unwrap(), br#"{"version":2}"#);
            assert_eq!(read_rustdoc_json(&path).unwrap()["version"], 2);
            assert_eq!(original_json["version"], 1);
            std::fs::remove_file(&path).unwrap();
            assert!(read_rustdoc(&path).is_err());
            assert!(read_rustdoc_json(&path).is_err());
            assert_eq!(&*first, br#"{"version":1}"#);
        });
        assert!(CACHE.with_borrow(|cache| cache.is_none()));
    }

    #[test]
    fn nested_and_unwinding_operations_restore_the_outer_cache() {
        with_rustdoc_cache(|| {
            extract(b"outer", || Ok(item(b"outer"))).unwrap();
            assert!(std::panic::catch_unwind(|| with_rustdoc_cache(|| {
                extract(b"inner", || Ok(item(b"inner"))).unwrap();
                panic!("negative control");
            }))
            .is_err());
            assert_eq!(
                extract(b"outer", || panic!("outer result was lost")).unwrap(),
                item(b"outer")
            );
            assert_eq!(
                CACHE.with_borrow(|cache| cache.as_ref().unwrap().rendered.borrow().len()),
                1
            );
        });
        assert!(CACHE.with_borrow(|cache| cache.is_none()));
    }
}
