use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    // Identity stays equal to the volume key; all I/O and parked streams retain
    // the native spelling, including after a rename through another alias.
    #[test]
    fn aliases_preserve_identity_and_native_paths(
        prefix in "[a-z]{0,3}",
        pair in prop::sample::select(vec![
            ("Dir", "dir"), ("ΟΣ", "ος"), ("Straße", "STRASSE"),
            ("ﬃ", "FFI"), ("İ", "i\u{307}"), ("ΐ", "ΐ"),
        ]),
        repetitions in 1..80usize,
    ) {
        let (projection, source) = folded::folded_projection()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let work = fuse_fixture_work();
        let repetitions = repetitions.min(240 / pair.0.len().max(pair.1.len()));
        let spelling = format!("{prefix}{}", pair.0.repeat(repetitions));
        let alias = format!("{prefix}{}", pair.1.repeat(repetitions));
        let original = name(&spelling);
        source.create_directory(&original, FileMetadata::default(), &work)?;
        source.create_directory(&original.child(b"Child".to_vec()), FileMetadata::default(), &work)?;
        source.create_directory(&name("new-parent"), FileMetadata::default(), &work)?;
        let parent = folded::lookup(&projection, ROOT_INODE, &spelling, &work)?;
        prop_assert_eq!(folded::lookup(&projection, ROOT_INODE, &alias, &work)?, parent);
        let child = folded::lookup(&projection, parent, "Child", &work)?;
        prop_assert_eq!(folded::lookup(&projection, parent, "child", &work)?, child);
        let destination = folded::lookup(&projection, ROOT_INODE, "new-parent", &work)?;
        {
            let mut state = projection.core.state().map_err(NativeMountError::from)?;
            for inode in [parent, child] {
                let stream = state.directory_stream(source.as_ref(), inode, 0, &work)?;
                let parking = state.streams.prepare_park(&work)?;
                state.streams.park(inode, stream, parking);
            }
        }
        projection.rename_name(ROOT_INODE, std::ffi::OsStr::new(&alias), destination,
            std::ffi::OsStr::new("Moved"), 0, &work)?;
        let mut state = projection.core.state().map_err(NativeMountError::from)?;
        for (inode, expected, parent_inode) in [
            (parent, name("new-parent").child(b"Moved".to_vec()), destination),
            (child, name("new-parent").child(b"Moved".to_vec()).child(b"child".to_vec()), parent),
        ] {
            let entry = state.by_inode.get(&inode).ok_or_else(|| TestCaseError::fail("missing inode"))?;
            for binding in entry.bindings.iter() {
                let (key, _) = source.folded_path_with_meter(binding.native_path(), &work)?
                    .ok_or_else(|| TestCaseError::fail("expected folded identity"))?;
                prop_assert_eq!(&*binding.path, &key);
                prop_assert_eq!(state.inode_by_path.get(&key), Some(&inode));
                let actual = source.lookup_with_meter(binding.native_path(), &work)?
                    .ok_or_else(|| TestCaseError::fail("native spelling no longer resolves"))?;
                prop_assert_eq!(actual.node.file_id, entry.lookup.node.file_id);
            }
            let stream = state.directory_stream(source.as_ref(), inode, 0, &work)?;
            prop_assert_eq!(&*stream.path, &expected);
            prop_assert_eq!(stream.parent_inode, parent_inode);
            source.read_directory_with_meter(&stream.path, None, 16, &work)?;
        }
    }
}
