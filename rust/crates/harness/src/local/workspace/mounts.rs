//! Default ownership policy, compiled through the common composed-view path.
use super::*;
use acyclic_fs::access::*;

pub(super) fn owner<T>(
    root: &Path,
    volume: &VolumeRef,
    source: T,
    volumes: impl IntoIterator<Item = (String, VolumeRef, T)>,
) -> Result<crate::workspace::MountTable<T>> {
    let principal = Principal {
        kind: PrincipalKind::Agent,
        id: "owner".into(),
        lineage: vec!["owner".into()],
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
    let actions = Action::ALL.into_iter().collect();
    let policy = Arc::new(Policy {
        schema_version: POLICY_SCHEMA_VERSION,
        version: 1,
        layers: vec![vec![Rule {
            id: "owner".into(),
            effect: Effect::Allow,
            principal: PrincipalPattern::default(),
            actions,
            resource: resource.clone(),
            conditions: Conditions {
                relation: Some(Relation::SelfTask),
                ..Conditions::default()
            },
        }]],
    });
    let volumes: Vec<_> = volumes.into_iter().collect();
    let capability = Capability {
        layers: vec![
            std::iter::once(volume)
                .chain(volumes.iter().map(|(_, volume, _)| volume))
                .map(|volume| Grant {
                    actions: Action::ALL.into_iter().collect(),
                    resource: ResourcePattern {
                        volume: Some(volume.id().into()),
                        ..resource.clone()
                    },
                })
                .collect(),
        ],
        expires_at: None,
    };
    let view = |name, volume, source| crate::workspace::VolumeView {
        name,
        agent: principal.id.clone(),
        volume,
        lineage: principal.lineage.clone(),
        generation: 0,
        head: 0,
        config: acyclic_fs::VolumeConfig::native(acyclic_fs::Lifecycle::Durable),
        policy: Arc::clone(&policy),
        source,
    };
    crate::workspace::compose(
        root.into(),
        &principal,
        &capability,
        Context {
            now: 0,
            approvals: 0,
        },
        view("project".into(), volume.clone(), source),
        volumes
            .into_iter()
            .map(|(name, volume, source)| view(name, volume, source)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        // Replacing a provider binding cannot change the namespace, access
        // decisions or volume identity compiled at composition time.
        #[test]
        fn binding_keeps_the_composed_authority(sources in any::<[u8; 2]>()) {
            let provider = crate::resources::ProviderRef::new(
                "binding".into(), "filesystem".into(), "1".into(), &mut None,
            ).unwrap();
            let volume = |name: &str| VolumeRef::new(provider.clone(), name.into(),
                VolumeClass::Project, VolumeOwner::Project("owner".into()), &mut None).unwrap();
            let root = std::env::current_dir().unwrap();
            let mut mounts = owner(&root, &volume("project"), sources[0],
                [("data".into(), volume("data"), sources[1])]).unwrap();
            let before: Vec<_> = mounts.mounts().map(|(path, mount)|
                (path.to_owned(), mount.view.volume.clone(), mount.mode, mount.view.source)).collect();
            for (_, mount) in mounts.mounts_mut() {
                mount.view.source = mount.view.source.wrapping_add(1);
            }
            for (path, volume, mode, source) in before {
                let (mounted, relative) = mounts.resolve(Path::new(&path)).unwrap();
                let qualifier = if path.is_empty() { "project" } else { path.strip_prefix(".volumes/").unwrap() };
                let (selected_path, selected) = mounts.named(Some(qualifier)).unwrap();
                prop_assert_eq!(selected_path, path.as_str());
                prop_assert_eq!(&selected.view.volume, &volume);
                prop_assert!(relative.is_empty());
                prop_assert_eq!(&mounted.view.volume, &volume);
                prop_assert_eq!(mounted.mode, mode);
                prop_assert_eq!(mounted.view.source, source.wrapping_add(1));
                prop_assert!(mounted.check(Action::Read, "file", 0).is_ok());
                prop_assert!(mounted.check(Action::Write, "file", 0).is_ok());
            }
            prop_assert!(mounts.named(Some("journal")).is_err());
            prop_assert!(mounts.named(Some("../data")).is_err());
            prop_assert_eq!(&mounts.named(None).unwrap().1.view.volume, &volume("project"));
        }
    }
}
