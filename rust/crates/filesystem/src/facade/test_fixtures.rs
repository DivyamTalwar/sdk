use super::*;

pub(crate) async fn legacy_folded_checkout()
-> Result<(MemoryCheckout, [FileId; 2]), Box<dyn std::error::Error>> {
    let fs = Fs::memory();
    let cancellation = CancellationToken::new();
    let config = VolumeConfig::portable(Lifecycle::Ephemeral);
    let volume = fs
        .create_volume(config, WorkBudget::UNBOUNDED, &cancellation)
        .await?
        .value;
    let mut checkout = volume
        .checkout(
            GenerationSelector::Head,
            CheckoutMode {
                access: AccessMode::ReadWrite,
                consistency: ConsistencyMode::Pinned,
                mutations: MutationMode::PrivateOverlay,
            },
            WorkBudget::UNBOUNDED,
            &cancellation,
        )
        .await?
        .value;
    let path = |name: &str| -> Result<NamespacePath, Box<dyn std::error::Error>> {
        Ok(NamespacePath::from_portable(
            &crate::path::PortablePath::parse(&format!("/{name}"), config.limits)?,
            config.limits,
        )?)
    };
    let mut mutations = Vec::new();
    for name in ["Straße".to_owned(), "STRASSE".to_owned()]
        .into_iter()
        .chain((0..255).map(|index| format!("Sa{index:03}")))
    {
        mutations.push(AuthoredMutation::CreateFile {
            path: path(&name)?,
            bytes: acyclic_stream::OwnedBytes::new(),
            metadata: FileMetadata::default(),
        });
    }
    checkout
        .apply_authored_transaction(mutations, WorkBudget::UNBOUNDED, &cancellation)
        .await?;
    let mut ids = [FileId::new(); 2];
    for (id, name) in ids.iter_mut().zip(["Straße", "STRASSE"]) {
        *id = checkout
            .lookup_no_follow(&path(name)?, WorkBudget::UNBOUNDED, &cancellation)
            .await?
            .value
            .record
            .ok_or("legacy file missing")?
            .file_id;
    }
    checkout.volume.config.case_sensitivity = CaseSensitivity::ProfileFolded;
    Ok((checkout, ids))
}
