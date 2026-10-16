use super::*;
use proptest::prelude::*;
use std::cell::Cell;

proptest! {
    #[test]
    fn producer_snapshots_follow_each_environment_and_end_with_the_operation(
        homes in prop::collection::vec("[a-z]{1,6}", 1..12),
    ) {
        let calls = Cell::new(0usize);
        let select = |home: &str| {
            let mut command = Command::new("rustup");
            command.env("RUSTUP_HOME", home);
            crate::with_command_environment(&command, || crate::render_cache::producer(|| {
                calls.set(calls.get()+1);
                Ok(RustdocProducer {
                    cargo: home.into(), targets: BTreeSet::from([home.into()]),
                    compiler: Compiler { rustc: home.into(), host: home.into(), cfgs: (home.into(), home.into()) },
                })
            }).unwrap())
        };
        crate::with_rustdoc_cache(|| {
            for home in &homes {
                let first = select(home);
                let second = select(home);
                prop_assert!(std::rc::Rc::ptr_eq(&first, &second));
                prop_assert_eq!(&first.cargo, &PathBuf::from(home));
                prop_assert_eq!(&first.compiler.host, home);
                prop_assert!(first.targets.contains(home));
            }
            prop_assert_eq!(calls.get(), homes.iter().collect::<BTreeSet<_>>().len());
            Ok(())
        })?;
        let before = calls.get();
        crate::with_rustdoc_cache(|| { select(&homes[0]); });
        prop_assert_eq!(calls.get(), before+1);
    }
}

#[cfg(unix)]
#[test]
fn scoped_rustup_homes_select_their_own_commands_targets_and_cfgs() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rustup");
    let path = std::env::join_paths(
        std::iter::once(fixture.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    crate::with_rustdoc_cache(|| {
        for name in ["a", "b", "a"] {
            let home = fixture.join(name);
            let mut command = Command::new("rustup");
            command.env("PATH", &path).env("RUSTUP_HOME", &home);
            crate::with_command_environment(&command, || {
                let host = format!("fixture-{name}");
                assert_eq!(rustdoc_host().unwrap(), host);
                assert!(rustdoc_targets(&[&host]).is_ok());
                assert_eq!(
                    nightly_rustdoc_command(&host).unwrap().get_program(),
                    home.join("bin/cargo")
                );
                assert!(rustdoc_platform_cfgs()
                    .unwrap()
                    .0
                    .contains(&format!("fixture_{name}")));
                let other = if name == "a" {
                    "fixture-b"
                } else {
                    "fixture-a"
                };
                assert!(matches!(
                    rustdoc_targets(&[other]),
                    Err(ProfileError::UnsupportedTarget { .. })
                ));
            });
        }
    });
}

#[cfg(unix)]
#[test]
fn a_new_operation_refreshes_installed_targets_in_the_same_home() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rustup");
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("bin")).unwrap();
    std::fs::write(home.path().join("bin/cargo"), "fixture Cargo").unwrap();
    std::os::unix::fs::symlink(fixtures.join("a/bin/rustc"), home.path().join("bin/rustc"))
        .unwrap();
    std::fs::write(home.path().join("targets"), "fixture-a\nfixture-added\n").unwrap();
    let path = std::env::join_paths(
        std::iter::once(fixtures).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut command = Command::new("rustup");
    command.env("PATH", path).env("RUSTUP_HOME", home.path());
    crate::with_command_environment(&command, || {
        crate::with_rustdoc_cache(|| {
            assert!(rustdoc_targets(&["fixture-added"]).is_ok());
        });
        std::fs::write(home.path().join("targets"), "fixture-a\n").unwrap();
        crate::with_rustdoc_cache(|| {
            assert!(matches!(
                rustdoc_targets(&["fixture-added"]),
                Err(ProfileError::UnsupportedTarget { .. })
            ));
        });
    });
}
