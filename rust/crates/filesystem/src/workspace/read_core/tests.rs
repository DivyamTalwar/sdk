use super::*;
use crate::Fs;

async fn comparison_peak(
    workspace: &Workspace<crate::MemoryAuthorityBackend, crate::MemoryObjectBackend>,
    selected: GenerationId,
    original: GenerationId,
    tracked: bool,
) -> Result<u64, WorkspaceError> {
    let cancellation = CancellationToken::new();
    let mut accounting = KernelAccounting::standalone(WorkBudget::UNBOUNDED, &cancellation);
    let (mut selected, path) = workspace
        .open_read_path(
            GenerationSelector::Exact(selected),
            "/entry",
            &mut accounting,
            &cancellation,
        )
        .await?;
    selected
        .value
        .lookup_no_follow_with_accounting(&path.value, &mut accounting, &cancellation, true)
        .await?;
    if tracked {
        let mut original = workspace
            .open_checkout(
                GenerationSelector::Exact(original),
                CheckoutMode::read_only_pinned(),
                &mut accounting,
                &cancellation,
            )
            .await?;
        original
            .value
            .lookup_no_follow_with_accounting(&path.value, &mut accounting, &cancellation, false)
            .await?;
        original.retire(&mut accounting)?;
    } else {
        // Negative control: the old path retained a checkout without reserving it.
        let mut original = workspace
            .checkout_accounted(
                GenerationSelector::Exact(original),
                CheckoutMode::read_only_pinned(),
                &mut accounting,
                &cancellation,
            )
            .await?;
        original
            .lookup_no_follow_with_accounting(&path.value, &mut accounting, &cancellation, false)
            .await?;
    }
    Ok(accounting
        .standalone_parts()
        .ok_or(crate::WorkError::Overflow)?
        .1
        .peak_allocation_bytes)
}

#[tokio::test]
async fn cold_original_checkout_counts_against_the_live_memory_ceiling()
-> Result<(), Box<dyn std::error::Error>> {
    let fs = Fs::memory();
    let cancellation = CancellationToken::new();
    let workspace = fs
        .create_workspace(
            "original-checkout-budget",
            WorkBudget::UNBOUNDED,
            &cancellation,
        )
        .await?
        .value;
    workspace.write_text("/entry", "payload").await?;
    let original = workspace
        .head(WorkBudget::UNBOUNDED, &cancellation)
        .await?
        .value;
    workspace.write_text("/unrelated", "new generation").await?;
    let selected = workspace
        .head(WorkBudget::UNBOUNDED, &cancellation)
        .await?
        .value;
    let broken_peak = comparison_peak(&workspace, selected.id(), original.id(), false).await?;
    let owned_peak = comparison_peak(&workspace, selected.id(), original.id(), true).await?;
    assert!(owned_peak > broken_peak, "{owned_peak} <= {broken_peak}");
    // The old implementation admitted this ceiling despite retaining more bytes.
    let denied = selected
        .read_range_if_unchanged(
            &original,
            "/entry",
            crate::ByteRange {
                offset: 0,
                length: 0,
            },
            WorkBudget {
                peak_allocation_bytes: broken_peak,
                ..WorkBudget::UNBOUNDED
            },
            &cancellation,
        )
        .await;
    assert!(
        matches!(&denied, Err(failure) if matches!(&failure.error,
            WorkspaceError::Work(crate::WorkError::BudgetExceeded {
                counter: "peak_allocation_bytes", ..
            }) | WorkspaceError::Filesystem(FsError::Work(crate::WorkError::BudgetExceeded {
                counter: "peak_allocation_bytes", ..
            }))
        )),
        "original checkout escaped the live allocation ceiling: {denied:?}"
    );
    let admitted = selected
        .read_range_if_unchanged(
            &original,
            "/entry",
            crate::ByteRange {
                offset: 0,
                length: 0,
            },
            WorkBudget {
                peak_allocation_bytes: owned_peak,
                ..WorkBudget::UNBOUNDED
            },
            &cancellation,
        )
        .await?;
    assert!(admitted.value.bytes.is_empty());
    assert!(admitted.work.peak_allocation_bytes <= owned_peak);
    Ok(())
}
