use super::*;
use proptest::prelude::*;

fn source() -> RegistryOwnerSource {
    RegistryOwnerSource {
        revision: "a".repeat(40),
        input_sha256: BTreeMap::new(),
        symlink_paths: BTreeSet::new(),
        discovery: CargoDiscoveryInputs::default(),
        owners: BTreeSet::new(),
        outside_release_authority: BTreeSet::new(),
        uses_frontier_authority: false,
        absent_authority_members: BTreeSet::new(),
    }
}

proptest! {
    #[test]
    fn owner_metadata_key_binds_source_config_tools_environment_and_location(
        value in ".{0,24}", axis in 0u8..7,
    ) {
        let mut source = source();
        let root = Path::new("root");
        let original = owner_metadata_context(root, &source, BTreeMap::new()).unwrap();
        let mut environment = BTreeMap::new();
        let mut location = root.to_owned();
        match axis {
            0 => { source.input_sha256.insert("Cargo.toml".into(), value); }
            1 => { source.discovery.config_sha256.insert("config".into(), Some(value)); }
            2 => source.discovery.cargo_sha256 = format!("cargo:{value}"),
            3 => source.discovery.cargo_version = format!("version:{value}"),
            4 => { source.symlink_paths.insert(value); }
            5 => { environment.insert("RUSTC".into(), value.into()); }
            6 => { location.push(format!("other{value}")); }
            _ => unreachable!(),
        }
        prop_assert_ne!(&original, &owner_metadata_context(&location, &source, environment).unwrap());
        // The old manifest-only context cannot distinguish any of these inputs.
        let broken = |_: &RegistryOwnerSource| root.join("Cargo.toml");
        prop_assert_eq!(broken(&source), broken(&self::source()));
    }
}

#[test]
fn concrete_owner_queries_reuse_only_within_a_run_and_wrappers_stay_fresh() {
    let directory = tempfile::tempdir().unwrap();
    let cargo = directory.path().join("cargo");
    fs::write(&cargo, "concrete compiler identity").unwrap();
    let root = directory.path();
    let mut source = source();
    let mut calls = 0;
    let mut query = || {
        calls += 1;
        serde_json::from_value(serde_json::json!({
            "packages": [], "workspace_members": [], "workspace_default_members": [],
            "resolve": null, "workspace_root": root, "target_directory": root.join("target"),
            "build_directory": null, "metadata": null, "version": 1,
        }))
        .map_err(|error| error.to_string())
    };
    crate::file_cache::begin_run();
    crate::file_cache::resolved_cargo(&cargo);
    owner_metadata(root, &cargo, &source, &mut query).unwrap();
    owner_metadata(root, &cargo, &source, &mut query).unwrap();
    source
        .input_sha256
        .insert("Cargo.toml".into(), "changed".into());
    owner_metadata(root, &cargo, &source, &mut query).unwrap();
    crate::file_cache::begin_run();
    crate::file_cache::resolved_cargo(&cargo);
    owner_metadata(root, &cargo, &source, &mut query).unwrap();
    crate::file_cache::begin_run();
    owner_metadata(root, &cargo, &source, &mut query).unwrap();
    owner_metadata(root, &cargo, &source, &mut query).unwrap();
    assert_eq!(
        calls, 5,
        "one reused concrete query; changed inputs, runs and proxies query afresh"
    );
}
