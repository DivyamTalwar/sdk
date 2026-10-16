use acyclic_fs::access::*;
use acyclic_fs::model::{CheckoutMode, GenerationSelector};
use acyclic_fs::{CancellationToken, Fs, WorkBudget};
use acyclic_harness::{
    AgentId, Capabilities, IdempotencyKey,
    conversation::{
        ContentGrant, ContentResidencyVerifier, FileRef, VolumeClass, VolumeOperation, VolumeOwner,
        VolumeRef,
    },
    core::{AggregateKind, Authority, AuthorityIssuer, Scope},
    filesystem::{FilesystemContentVerifier, FilesystemHost},
    resources::ProviderRef,
};
use proptest::prelude::*;
use std::{error::Error, sync::Arc};

#[path = "support/content_context.rs"]
mod context;

struct Fixture {
    fs: acyclic_fs::MemoryFs,
    host: Arc<FilesystemHost<acyclic_fs::MemoryAuthorityBackend, acyclic_fs::MemoryObjectBackend>>,
    issuer: AuthorityIssuer,
    volume: VolumeRef,
    scope: Scope,
    file: FileRef,
}

impl Fixture {
    async fn new() -> Result<Self, Box<dyn Error>> {
        Self::with_policy(true).await
    }

    async fn with_policy(initialize: bool) -> Result<Self, Box<dyn Error>> {
        let fs = Fs::memory();
        let provider = ProviderRef::new(
            "access-test".into(),
            "filesystem".into(),
            "2".into(),
            &mut None,
        )?;
        let host = FilesystemHost::new(fs.clone(), &provider, None)?;
        let volume = VolumeRef::new(
            provider,
            "project".into(),
            VolumeClass::Project,
            VolumeOwner::Project("owner".into()),
            &mut None,
        )?;
        if initialize {
            host.create_volume(&volume).await?;
        } else {
            fs.create_workspace(
                volume.storage_name(&mut None)?,
                WorkBudget::UNBOUNDED,
                &CancellationToken::new(),
            )
            .await?;
        }
        let issuer = AuthorityIssuer::new(
            "owner",
            [19; 32],
            Authority {
                kind: AggregateKind::Task,
                id: "read-task".into(),
            },
        );
        let scope = issuer.issue(
            "legacy",
            &Capabilities::new([
                volume.capability(VolumeOperation::Read, &mut None)?,
                volume.capability(VolumeOperation::Write, &mut None)?,
            ]),
            Some(AgentId::from_bytes([7; 16])),
            None,
            &mut None,
        )?;
        let writer = ContentGrant::verify(
            &issuer.verifier(&mut None)?,
            &scope,
            &volume,
            VolumeOperation::Write,
            4096,
            &mut None,
        )?;
        let file = host
            .put_content(
                &volume,
                &writer,
                "secrets/key",
                b"private",
                "text/plain",
                "key",
                4096,
                &IdempotencyKey::new("file")?,
                None,
                None,
            )
            .await?;
        Ok(Self {
            fs,
            host,
            issuer,
            volume,
            scope,
            file,
        })
    }

    fn reader(
        &self,
        scope: &Scope,
    ) -> Result<
        Arc<
            FilesystemContentVerifier<
                acyclic_fs::MemoryAuthorityBackend,
                acyclic_fs::MemoryObjectBackend,
            >,
        >,
        Box<dyn Error>,
    > {
        Ok(FilesystemContentVerifier::new(
            self.host.clone(),
            &self.issuer.verifier(&mut None)?,
            scope,
            4096,
            None,
        )?)
    }

    async fn policy(&self, deny: bool, version: u64) -> Result<(), Box<dyn Error>> {
        let mut policy: Policy =
            serde_json::from_slice(include_bytes!("../src/filesystem/access/initial.json"))?;
        policy.version = version;
        if deny {
            let mut rule = policy.layers[0][0].clone();
            rule.id = "hidden-secret".into();
            rule.effect = Effect::Deny;
            rule.actions = [Action::Read].into_iter().collect();
            rule.resource.path = "secrets/**".into();
            policy.layers[0].push(rule);
        }
        let cancellation = CancellationToken::new();
        let workspace = self
            .fs
            .open_workspace(
                self.volume.storage_name(&mut None)?,
                WorkBudget::UNBOUNDED,
                &cancellation,
            )
            .await?
            .value;
        let mut checkout = workspace
            .checkout(
                GenerationSelector::Head,
                CheckoutMode::tracking_transaction(),
                WorkBudget::UNBOUNDED,
                &cancellation,
            )
            .await?
            .value;
        checkout
            .write_access_policy(
                &serde_json::to_vec(&policy)?,
                acyclic_fs::NamedAttributeWriteMode::Replace,
                WorkBudget::UNBOUNDED,
                &cancellation,
            )
            .await?;
        checkout
            .commit(
                acyclic_fs::OperationId::new(),
                None,
                None,
                None,
                acyclic_fs::PublicationPermit::Unrestricted,
                WorkBudget::UNBOUNDED,
                &cancellation,
                |_: WorkBudget| {
                    std::future::ready(Ok::<_, acyclic_fs::FsError>(acyclic_fs::OperationReceipt {
                        value: (),
                        work: Default::default(),
                    }))
                },
            )
            .await?;
        Ok(())
    }

    fn narrowed(&self, grants_path: bool, expired: bool) -> Result<Scope, Box<dyn Error>> {
        let capability = Capability {
            layers: vec![vec![Grant {
                actions: [Action::Read, Action::ReadHistory].into_iter().collect(),
                resource: ResourcePattern {
                    lineage_root: None,
                    volume: None,
                    generations: Generations::All,
                    path: if grants_path { "**" } else { "public/**" }.into(),
                },
            }]],
            expires_at: expired.then_some(1),
        };
        Ok(self.issuer.issue(
            "ceiling",
            &Capabilities::new([
                self.volume.capability(VolumeOperation::Read, &mut None)?,
                capability.to_scope_label()?,
            ]),
            self.scope.agent(),
            None,
            &mut None,
        )?)
    }
}

#[tokio::test]
async fn legacy_task_volume_grant_cannot_bypass_persisted_denial() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    let reader = fixture.reader(&fixture.scope)?;
    let context =
        context::content_context(reader.clone(), None, fixture.scope.capabilities().clone())
            .await?;
    assert_eq!(
        context.read_file(&fixture.file, None).await?.as_ref(),
        b"private"
    );
    fixture.policy(true, 2).await?;
    assert!(matches!(
        context.read_file(&fixture.file, None).await,
        Err(acyclic_harness::Error::Unauthorized(_))
    ));
    fixture.policy(false, 3).await?;
    assert_eq!(
        context.read_file(&fixture.file, None).await?.as_ref(),
        b"private"
    );
    Ok(())
}

#[tokio::test]
async fn legacy_label_cannot_bypass_capability_path_or_expiry() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    for (grants_path, expired, allowed) in [
        (true, false, true),
        (false, false, false),
        (true, true, false),
    ] {
        let scope = fixture.narrowed(grants_path, expired)?;
        assert_eq!(
            fixture
                .reader(&scope)?
                .read(&fixture.file, 4096, None)?
                .await
                .is_ok(),
            allowed
        );
    }
    Ok(())
}

#[tokio::test]
async fn content_reads_fail_closed_without_a_persisted_policy() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::with_policy(false).await?;
    assert!(matches!(
        fixture
            .reader(&fixture.scope)?
            .read(&fixture.file, 4096, None)?
            .await,
        Err(acyclic_harness::Error::Unauthorized(_))
    ));
    Ok(())
}

#[tokio::test]
async fn structured_only_reads_and_ambiguous_payloads_use_the_same_gate()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    let allow = Capability {
        layers: vec![vec![Grant {
            actions: [Action::Read, Action::ReadHistory].into_iter().collect(),
            resource: ResourcePattern {
                lineage_root: None,
                volume: None,
                generations: Generations::All,
                path: "**".into(),
            },
        }]],
        expires_at: None,
    };
    let only = fixture.issuer.issue(
        "structured",
        &Capabilities::new([allow.to_scope_label()?]),
        fixture.scope.agent(),
        None,
        &mut None,
    )?;
    assert_eq!(
        fixture
            .reader(&only)?
            .read(&fixture.file, 4096, None)?
            .await?
            .as_ref(),
        b"private"
    );
    let narrower = allow.clone().attenuate(vec![], None);
    let ambiguous = fixture.issuer.issue(
        "ambiguous",
        &Capabilities::new([
            fixture
                .volume
                .capability(VolumeOperation::Read, &mut None)?,
            allow.to_scope_label()?,
            narrower.to_scope_label()?,
        ]),
        fixture.scope.agent(),
        None,
        &mut None,
    )?;
    assert!(matches!(
        fixture
            .reader(&ambiguous)?
            .read(&fixture.file, 4096, None)?
            .await,
        Err(acyclic_harness::Error::Unauthorized(_))
    ));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn production_read_is_policy_intersected_with_the_presented_ceiling(
        deny in any::<bool>(), grants_path in any::<bool>(), expired in any::<bool>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        runtime.block_on(async {
            let fixture = Fixture::new().await?;
            fixture.policy(deny, 2).await?;
            let scope = fixture.narrowed(grants_path, expired)?;
            let allowed = fixture.reader(&scope)?.read(&fixture.file, 4096, None)?.await.is_ok();
            assert_eq!(allowed, !deny && grants_path && !expired);
            Ok::<_, Box<dyn Error>>(())
        }).map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

#[cfg(feature = "test-support")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn bounded_access_records_roundtrip(
        path in "[a-zΣß?*]{1,12}",
        expiry in prop::option::of(any::<u64>()),
        malformed in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        let mut policy: Policy = serde_json::from_slice(include_bytes!("../src/filesystem/access/initial.json"))?;
        policy.layers[0][0].resource.path = path;
        policy.layers[0][0].conditions.expires_at = expiry;
        acyclic_harness::filesystem::check_access_record(&serde_json::to_vec(&policy)?)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        acyclic_harness::filesystem::check_access_record(&malformed)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}
