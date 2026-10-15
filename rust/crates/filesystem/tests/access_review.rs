use super::*;
use acyclic_fs::kernel::{LogicalName, NameEncoding};
use acyclic_fs::model::{CaseSensitivity, FilesystemProfile, UnicodePolicy};

fn native_exact_grant(
    profile: FilesystemProfile,
    capability_grant: bool,
) -> Result<(), InvalidPolicy> {
    let caller = principal();
    let context = Context {
        now: 100,
        approvals: 0,
    };
    let policy = super::policy(vec![rule(
        "allow",
        Effect::Allow,
        &[Action::Read],
        if capability_grant { "**" } else { "public" },
    )]);
    let cap = if capability_grant {
        capability().attenuate(
            vec![Grant {
                actions: [Action::Read].into_iter().collect(),
                resource: pattern("public"),
            }],
            None,
        )
    } else {
        capability()
    };
    let mut res = resource(&caller.lineage, "public", 10);
    res.config.profile = profile;
    res.config.case_sensitivity = CaseSensitivity::ProfileFolded;
    let encoding = match profile {
        FilesystemProfile::Posix => NameEncoding::PosixBytes,
        FilesystemProfile::Windows => NameEncoding::WindowsUtf16Le,
        _ => return Err(InvalidPolicy("fixture requires a native profile")),
    };
    let name = |text: &str| {
        LogicalName::new(
            encoding,
            if encoding == NameEncoding::WindowsUtf16Le {
                text.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else {
                text.as_bytes().to_vec()
            },
            res.config.limits.maximum_component_bytes,
        )
        .map_err(|_| InvalidPolicy("invalid fixture name"))
    };
    assert_ne!(
        name("public")?.case_fold_key(),
        name("PUBLIC")?.case_fold_key()
    );
    let trie = PathTrie::compile(&policy, &caller, res, context, &cap)?;
    for (path, allowed) in [("public", true), ("PUBLIC", false)] {
        res.path = path;
        let reference = decide(&policy, &caller, Action::Read, res, context, &cap);
        assert_eq!(
            reference.allowed, allowed,
            "{profile:?}, capability={capability_grant}: {path}"
        );
        assert_eq!(trie.decide(Action::Read, path, 100), reference);
        assert_eq!(
            trie.prepare("")?
                .child_name(&name(path)?)?
                .decide(Action::Read, 100),
            reference
        );
    }
    Ok(())
}

#[test]
fn posix_policy_grants_preserve_native_identity() -> Result<(), InvalidPolicy> {
    native_exact_grant(FilesystemProfile::Posix, false)
}

#[test]
fn posix_capability_grants_preserve_native_identity() -> Result<(), InvalidPolicy> {
    native_exact_grant(FilesystemProfile::Posix, true)
}

#[test]
fn windows_policy_grants_preserve_native_identity() -> Result<(), InvalidPolicy> {
    native_exact_grant(FilesystemProfile::Windows, false)
}

#[test]
fn windows_capability_grants_preserve_native_identity() -> Result<(), InvalidPolicy> {
    native_exact_grant(FilesystemProfile::Windows, true)
}

#[test]
fn native_name_views_reject_unsupported_encodings_and_names() -> Result<(), InvalidPolicy> {
    let caller = principal();
    let policy = super::policy(vec![rule("allow", Effect::Allow, &Action::ALL, "**")]);
    let cap = capability();
    let context = Context {
        now: 100,
        approvals: 0,
    };
    for (profile, encoding, invalid) in [
        (
            FilesystemProfile::Posix,
            NameEncoding::PosixBytes,
            vec![0xff],
        ),
        (
            FilesystemProfile::Windows,
            NameEncoding::WindowsUtf16Le,
            vec![0, 0xd8],
        ),
    ] {
        let mut res = resource(&caller.lineage, "", 10);
        res.config.profile = profile;
        res.config.unicode = UnicodePolicy::RequireNfc;
        let trie = PathTrie::compile(&policy, &caller, res, context, &cap)?;
        let root = trie.prepare("")?;
        let utf8 = LogicalName::new(NameEncoding::Utf8, b"public".to_vec(), 255)
            .map_err(|_| InvalidPolicy("invalid fixture name"))?;
        assert!(root.child_name(&utf8).is_err());
        let non_unicode = LogicalName::new(encoding, invalid, 255)
            .map_err(|_| InvalidPolicy("invalid fixture name"))?;
        assert!(root.child_name(&non_unicode).is_err());
        // NFC admission applies only to UTF-8 logical names in the filesystem.
        res.path = "e\u{301}";
        assert!(decide(&policy, &caller, Action::Read, res, context, &cap).allowed);
        assert!(trie.decide(Action::Read, res.path, 100).allowed);
    }
    Ok(())
}

#[test]
fn windows_access_paths_obey_encoded_name_and_path_limits() -> Result<(), InvalidPolicy> {
    let caller = principal();
    let policy = super::policy(vec![rule("allow", Effect::Allow, &Action::ALL, "**")]);
    let cap = capability();
    let context = Context {
        now: 100,
        approvals: 0,
    };
    let mut res = resource(&caller.lineage, "", 10);
    res.config.profile = FilesystemProfile::Windows;
    res.config.limits.maximum_component_bytes = 4;
    res.config.limits.maximum_path_bytes = 8;
    let trie = PathTrie::compile(&policy, &caller, res, context, &cap)?;
    for (path, allowed) in [
        ("ab", true),
        ("abc", false),
        ("ab/c", true),
        ("ab/cd", false),
        ("😀", true),
    ] {
        res.path = path;
        assert_eq!(
            decide(&policy, &caller, Action::Read, res, context, &cap).allowed,
            allowed,
            "{path}"
        );
        assert_eq!(
            trie.decide(Action::Read, path, 100).allowed,
            allowed,
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn volume_name_keys_apply_to_rules_and_capabilities() -> Result<(), InvalidPolicy> {
    let caller = principal();
    let context = Context {
        now: 100,
        approvals: 0,
    };
    let cap = capability();
    for (deny, alias) in [
        ("secrets/**", "Secrets/key"),
        ("ÉCOLE/**", "école/key"),
        ("ΟΣ/**", "ος/key"),
        ("İ/**", "i\u{307}/key"),
    ] {
        let policy = super::policy(vec![
            rule("allow", Effect::Allow, &Action::ALL, "**"),
            rule("hidden", Effect::Deny, &[Action::List, Action::Read], deny),
        ]);
        let mut res = resource(&caller.lineage, alias, 10);
        res.config.case_sensitivity = CaseSensitivity::ProfileFolded;
        let trie = PathTrie::compile(&policy, &caller, res, context, &cap)?;
        for action in [Action::List, Action::Read] {
            let reference = decide(&policy, &caller, action, res, context, &cap);
            assert!(!reference.allowed, "{deny}: {alias}");
            assert_eq!(reference.rule_id, Some("hidden"));
            assert_eq!(trie.prepare(alias)?.decide(action, 100), reference);
        }
    }
    let policy = super::policy(vec![
        rule("allow", Effect::Allow, &Action::ALL, "**"),
        rule("hidden", Effect::Deny, &[Action::Read], "secrets/**"),
    ]);
    let res = resource(&caller.lineage, "Secrets/key", 10);
    assert!(decide(&policy, &caller, Action::Read, res, context, &cap).allowed);
    let cap = cap.attenuate(
        vec![Grant {
            actions: [Action::Read].into_iter().collect(),
            resource: pattern("SECRETS/**"),
        }],
        None,
    );
    let mut res = resource(&caller.lineage, "secrets/key", 10);
    res.config.case_sensitivity = CaseSensitivity::ProfileFolded;
    let policy = super::policy(vec![rule("allow", Effect::Allow, &Action::ALL, "**")]);
    assert!(decide(&policy, &caller, Action::Read, res, context, &cap).allowed);
    assert!(
        PathTrie::compile(&policy, &caller, res, context, &cap)?
            .prepare(res.path)?
            .decide(Action::Read, 100)
            .allowed
    );
    Ok(())
}

#[test]
fn unicode_admission_matches_the_volume() -> Result<(), InvalidPolicy> {
    let caller = principal();
    let policy = super::policy(vec![rule("allow", Effect::Allow, &Action::ALL, "**")]);
    let cap = capability();
    let context = Context {
        now: 100,
        approvals: 0,
    };
    for (unicode, path, allowed) in [
        (UnicodePolicy::RequireNfc, "é/key", true),
        (UnicodePolicy::RequireNfc, "e\u{301}/key", false),
        (UnicodePolicy::Preserve, "e\u{301}/key", true),
    ] {
        let mut res = resource(&caller.lineage, path, 10);
        res.config.unicode = unicode;
        let trie = PathTrie::compile(&policy, &caller, res, context, &cap)?;
        assert_eq!(
            decide(&policy, &caller, Action::Read, res, context, &cap).allowed,
            allowed
        );
        assert_eq!(trie.decide(Action::Read, path, 100).allowed, allowed);
    }
    Ok(())
}

#[test]
fn review_exponential_globs_are_bounded_in_policy_and_delegation() {
    let caller = principal();
    let context = Context {
        now: 100,
        approvals: 0,
    };
    let adversarial = format!("**/a{}", "/?".repeat(24));
    let policy = super::policy(vec![rule("allow", Effect::Allow, &Action::ALL, "**")]);
    let mut restricted = policy.clone();
    restricted.layers.push(vec![rule(
        "expansion",
        Effect::Allow,
        &Action::ALL,
        &adversarial,
    )]);
    let cap = capability();
    assert!(
        PathTrie::compile(
            &restricted,
            &caller,
            resource(&caller.lineage, "", 10),
            context,
            &cap
        )
        .is_err()
    );
    let delegated = cap.attenuate(
        vec![Grant {
            actions: Action::ALL.into_iter().collect(),
            resource: pattern(&adversarial),
        }],
        None,
    );
    assert!(
        PathTrie::compile(
            &policy,
            &caller,
            resource(&caller.lineage, "", 10),
            context,
            &delegated
        )
        .is_err()
    );
}

#[test]
fn prepared_paths_retain_deadlines_and_view_identity() -> Result<(), InvalidPolicy> {
    let caller = principal();
    let policy = super::policy(vec![rule("allow", Effect::Allow, &Action::ALL, "**")]);
    let mut cap = capability();
    cap.expires_at = Some(110);
    let context = Context {
        now: 100,
        approvals: 0,
    };
    let trie = PathTrie::compile(
        &policy,
        &caller,
        resource(&caller.lineage, "", 10),
        context,
        &cap,
    )?;
    assert_eq!(trie.valid_until(), Some(110));
    let path = trie
        .prepare("")?
        .child("agents")?
        .child("worker")?
        .child("é.txt")?;
    assert!(path.decide(Action::Read, 109).allowed);
    assert!(!path.decide(Action::Read, 110).allowed);
    assert!(!path.decide(Action::Read, 99).allowed);
    assert_eq!(
        path.decide(Action::Read, 109),
        trie.decide(Action::Read, "agents/worker/é.txt", 109)
    );
    let revoked = Policy {
        version: 2,
        layers: Vec::new(),
        ..policy
    };
    let revoked = PathTrie::compile(
        &revoked,
        &caller,
        resource(&caller.lineage, "", 10),
        context,
        &cap,
    )?;
    assert!(
        !revoked
            .prepare("agents/worker/é.txt")?
            .decide(Action::Read, 109)
            .allowed
    );
    assert!(path.decide(Action::Read, 109).allowed);
    assert!(trie.prepare("a").and_then(|p| p.child("../b")).is_err());
    assert!(trie.prepare("a//b").is_err());
    Ok(())
}
