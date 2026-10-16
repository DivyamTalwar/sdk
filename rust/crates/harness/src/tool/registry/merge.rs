use super::*;

impl ToolRegistry {
    /// Adds a disjoint registry, retaining every pinned revision and model selection.
    pub fn extend(&mut self, other: Self) -> Result<()> {
        if self
            .data
            .versions
            .keys()
            .any(|key| other.data.versions.contains_key(key))
            || self
                .data
                .selected
                .keys()
                .any(|name| other.data.selected.contains_key(name))
        {
            return Err(Error::Conflict(
                "tool registries contain the same logical name".into(),
            ));
        }
        if self
            .data
            .catalog_revisions
            .keys()
            .any(|name| other.data.catalog_revisions.contains_key(name))
        {
            return Err(Error::Conflict(
                "tool registries contain the same catalog namespace".into(),
            ));
        }
        let data = Arc::make_mut(&mut self.data);
        let mut other = Arc::unwrap_or_clone(other.data);
        data.versions.append(&mut other.versions);
        data.selected.append(&mut other.selected);
        data.catalog_revisions.append(&mut other.catalog_revisions);
        Ok(())
    }
}
