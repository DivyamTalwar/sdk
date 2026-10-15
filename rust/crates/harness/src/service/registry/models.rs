use super::*;

fn registry_model(broken: bool) {
    loom::model(move || {
        let registry = Arc::new(Registry::new());
        let first = registry.clone();
        let insert = loom::thread::spawn(move || first.insert("workspace".into(), Arc::new(1)));
        if broken {
            registry
                .hosts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
        let second = registry.insert("workspace".into(), Arc::new(2));
        let joined = insert.join();
        assert!(joined.is_ok(), "registry model worker panicked");
        let Ok(first) = joined else {
            return;
        };
        assert_ne!(
            first.is_ok(),
            second.is_ok(),
            "registration admitted two owners"
        );
        let selected = registry.get("workspace");
        assert!(selected.is_ok(), "registered host disappeared");
        if let Ok(selected) = selected {
            assert_eq!(*selected, if first.is_ok() { 1 } else { 2 });
        }
    });
}

#[test]
fn concurrent_registration_retains_exactly_one_owner() {
    registry_model(false);
}

#[test]
#[should_panic(expected = "registration admitted two owners")]
fn negative_control_detects_replacing_a_registered_owner() {
    registry_model(true);
}
