use super::*;

pub(super) fn execute_default_profiles(
    root: &Path,
    output: &Path,
    rustdoc_dir: &Path,
    metadata: &Metadata,
    source_sha256: &str,
    cargo: &Path,
) -> Result<Vec<ExecutedProfile>, CliError> {
    let published = published_packages(root)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let packages = metadata
        .packages
        .iter()
        .filter(|package| {
            published.contains(package.name.as_ref())
                || api_owner_for_package(metadata, package.name.as_ref())
                    .is_ok_and(|owner| owner.rustdoc_package == package.name.as_ref())
        })
        .collect::<Vec<_>>();
    let disjoint = distinct_receipt_names(
        packages
            .iter()
            .flat_map(|package| historical_targets(package))
            .map(|(target, _)| target.name.as_str()),
    );
    let executed = if packages.len() < 2 || !disjoint {
        compile_default_profiles(root, output, rustdoc_dir, metadata, cargo, None)?
    } else {
        let jobs = packages
            .iter()
            .map(|package| package.name.to_string())
            .collect();
        let (root, output, rustdoc_dir, cargo) = (
            root.to_owned(),
            output.to_owned(),
            rustdoc_dir.to_owned(),
            cargo.to_owned(),
        );
        let metadata = metadata.clone();
        rustc_host()?;
        crate::parallel::map(
            jobs,
            sdk_docs::rustdoc_worker(move |owner| {
                compile_default_profiles(
                    &root,
                    &output,
                    &rustdoc_dir,
                    &metadata,
                    &cargo,
                    Some(&owner),
                )
            }),
        )
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect()
    };
    // Aliases share one index. All workers, including failures, join first.
    for profile in &executed {
        retain_profile_generated_sources(root, &profile.receipt)?;
        fs::write(
            profile.receipt.with_extension("source.sha256"),
            source_sha256,
        )
        .map_err(io_error)?;
    }
    Ok(executed)
}
