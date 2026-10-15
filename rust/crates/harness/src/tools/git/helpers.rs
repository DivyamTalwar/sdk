use super::*;
/// Resolves a path argument within the managed workspace.
pub(crate) fn workspace_path_argument(mount: &Path, argument: &str) -> Result<String, Error> {
    let argument = Path::new(argument);
    let relative = if argument.is_absolute() {
        argument
            .strip_prefix(mount)
            .map_err(|_| Error::Invalid("path is outside the managed workspace".to_owned()))?
    } else {
        argument
    };
    let mut result = String::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| {
                    Error::Invalid("compatibility path is not valid Unicode".to_owned())
                })?;
                result.push('/');
                result.push_str(name);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Invalid(
                    "path escapes the managed workspace".to_owned(),
                ));
            }
        }
    }
    if result.is_empty() {
        return Err(Error::Invalid(
            "path must name a file inside the managed workspace".to_owned(),
        ));
    }
    Ok(result)
}

pub(super) fn existing_tracked_paths<'a>(
    generation: &'a LocalGeneration,
    candidates: &'a BTreeSet<String>,
) -> impl std::future::Future<Output = Result<BTreeSet<String>, Error>> + Send + 'a {
    async move {
        let mut tracked = BTreeSet::new();
        for path in candidates {
            match generation
                .stat(
                    &format!("/{path}"),
                    acyclic_fs::WorkBudget::UNBOUNDED,
                    &acyclic_fs::CancellationToken::new(),
                )
                .await
                .map(|receipt| receipt.value)
                .map_err(|failure| failure.error)
            {
                Ok(stat) if stat.kind != FileKind::Directory => {
                    tracked.insert(path.clone());
                }
                Ok(_) | Err(WorkspaceError::NotFound) => {}
                Err(error) => return Err(Error::from(error)),
            }
        }
        Ok(tracked)
    }
}
