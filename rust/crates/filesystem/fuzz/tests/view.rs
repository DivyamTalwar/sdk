use acyclic_fs::access::*;
use acyclic_fs::kernel::NamespacePath;

#[path = "../../src/access/view.rs"]
mod published;

fn compiled(version: u64, allow: bool, volume: &str) -> PathTrie {
    let principal = Principal {
        kind: PrincipalKind::Service,
        id: "owner".into(),
        lineage: vec![],
        groups: Default::default(),
        roles: Default::default(),
        tags: Default::default(),
        attributes: Default::default(),
    };
    let resource = ResourcePattern {
        lineage_root: None,
        volume: None,
        generations: Generations::All,
        path: "**".into(),
    };
    let rule = Rule {
        id: "read".into(),
        effect: Effect::Allow,
        principal: PrincipalPattern::default(),
        actions: [Action::Read].into_iter().collect(),
        resource: resource.clone(),
        conditions: Conditions::default(),
    };
    PathTrie::compile(
        &Policy {
            schema_version: POLICY_SCHEMA_VERSION,
            version,
            layers: if allow { vec![vec![rule]] } else { vec![] },
        },
        &principal,
        Resource {
            config: acyclic_fs::VolumeConfig::portable(acyclic_fs::Lifecycle::Durable),
            lineage: &[],
            volume,
            generation: 1,
            head: 1,
            path: "",
        },
        Context {
            now: 0,
            approvals: 0,
        },
        &Capability {
            layers: vec![vec![Grant {
                actions: [Action::Read].into_iter().collect(),
                resource,
            }]],
            expires_at: None,
        },
    )
    .expect("bounded model policy compiles")
}

#[test]
fn publication_is_atomic_and_pinned_handles_retain_their_view() {
    loom::model(|| {
        let views = loom::sync::Arc::new(published::AccessView::new(compiled(1, true, "project")));
        let initial = views.current().expect("initial view");
        let root = NamespacePath::new(Vec::new(), initial.config().limits).expect("root");
        let pinned = initial.pin(&root).expect("pinned handle");
        let publisher = loom::sync::Arc::clone(&views);
        let writer = loom::thread::spawn(move || {
            publisher
                .publish(compiled(2, false, "project"))
                .expect("publish revision");
        });
        let observed = views.current().expect("concurrent open");
        assert!(matches!(
            (
                observed.policy_version(),
                observed.decide(Action::Read, "", 0).allowed
            ),
            (1, true) | (2, false)
        ));
        writer.join().expect("publisher finishes without deadlock");
        assert!(
            !views
                .current()
                .expect("new open")
                .decide(Action::Read, "", 0)
                .allowed
        );
        assert!(pinned.decide(Action::Read, 0).allowed);
    });
}

#[test]
fn racing_revisions_cannot_roll_back_or_replace_the_binding() {
    loom::model(|| {
        let views = loom::sync::Arc::new(published::AccessView::new(compiled(1, true, "project")));
        let publisher = loom::sync::Arc::clone(&views);
        let writer = loom::thread::spawn(move || {
            let _stale_or_installed = publisher.publish(compiled(2, true, "project"));
            assert!(
                publisher
                    .publish(compiled(4, true, "other-volume"))
                    .is_err()
            );
        });
        views
            .publish(compiled(3, false, "project"))
            .expect("latest revision");
        writer.join().expect("publisher finishes without deadlock");
        let current = views.current().expect("current view");
        assert_eq!(current.policy_version(), 3);
        assert!(!current.decide(Action::Read, "", 0).allowed);
        assert_eq!(current.volume(), "project");
    });
}
