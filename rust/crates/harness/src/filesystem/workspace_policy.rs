use crate::{Error, Result};
use acyclic_fs::facade::FsWork as _;
use acyclic_fs::{
    CancellationToken, DefaultTextMergeDriver, GitIgnorePolicy, LazyWorkspace,
    LocalAuthorityBackend, LocalCoreStateStore, LocalObjectBackend, MergeDriverRegistry,
    WorkBudget, demand::native::NativeDemandSource,
};
use std::sync::Arc;
type LocalLazyWorkspace = LazyWorkspace<
    LocalAuthorityBackend,
    LocalObjectBackend,
    NativeDemandSource,
    LocalCoreStateStore,
>;
fn display(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}
/// Reserved SDK metadata excluded from compatibility commits.
pub const RESERVED_IGNORE_RULES: &str = ".acyclic-sdk/\n.git/\n.volumes/\n";

/// Loads text merge drivers and workspace attributes.
async fn merge_drivers_for(workspace: &LocalLazyWorkspace) -> Result<Arc<MergeDriverRegistry>> {
    let attributes = match workspace
        .read(
            "/.gitattributes",
            1024 * 1024,
            &mut acyclic_fs::kernel::KernelAccounting::Standalone {
                allocations: Default::default(),
                work: acyclic_fs::WorkCounters::UNCHARGED,
                budget: WorkBudget::UNBOUNDED,
                cancellation: &CancellationToken::new(),
            },
        )
        .await
    {
        Ok(chunk) => String::from_utf8(chunk.bytes.to_vec())
            .map_err(|_| Error::Invalid(".gitattributes must be UTF-8".to_owned()))?,
        Err(acyclic_fs::LazyWorkspaceError::NotFound) => String::new(),
        Err(error) => return Err(display(error)),
    };
    let mut registry = MergeDriverRegistry::new();
    registry
        .register("text", Arc::new(DefaultTextMergeDriver))
        .map_err(display)?;
    registry.set_default("text").map_err(display)?;
    if !attributes.trim().is_empty() {
        registry.set_git_attributes(&attributes).map_err(display)?;
    }
    Ok(Arc::new(registry))
}

/// File-based ignore and merge policy for a managed workspace.
pub struct WorkspacePolicy {
    /// Excludes SDK metadata and composed volume mounts.
    pub ignore: GitIgnorePolicy,
    /// Text merge drivers configured by attributes.
    pub merge_drivers: Arc<MergeDriverRegistry>,
}
impl WorkspacePolicy {
    /// Loads workspace rules while retaining reserved SDK exclusions.
    pub async fn load(workspace: &LocalLazyWorkspace) -> Result<Self> {
        let ignore_text = match workspace
            .read(
                "/.gitignore",
                1024 * 1024,
                &mut WorkBudget::UNBOUNDED.accounting(&CancellationToken::new()),
            )
            .await
        {
            Ok(chunk) => String::from_utf8_lossy(&chunk.bytes).into_owned(),
            Err(acyclic_fs::LazyWorkspaceError::NotFound) => String::new(),
            Err(error) => return Err(display(error)),
        };
        Ok(Self {
            ignore: GitIgnorePolicy::parse(&format!("{ignore_text}\n{RESERVED_IGNORE_RULES}")),
            merge_drivers: merge_drivers_for(workspace).await?,
        })
    }
}
