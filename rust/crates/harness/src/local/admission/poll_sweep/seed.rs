//! Share only the immutable, durably created empty-volume fixture.
use super::*;

pub(super) struct Seed(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Seed {
    pub async fn new() -> Result<Self> {
        let fixture = Fixture::new(Scenario::Text, None).await?;
        let root = fixture.directory.path().join("filesystem");
        let mut pending = vec![PathBuf::new()];
        let mut entries = Vec::new();
        while let Some(relative) = pending.pop() {
            let path = root.join(&relative);
            if path.is_dir() {
                entries.push((relative.clone(), None));
                let mut children = std::fs::read_dir(path)
                    .map_err(storage)?
                    .map(|entry| entry.map(|entry| relative.join(entry.file_name())))
                    .collect::<std::io::Result<Vec<_>>>()
                    .map_err(storage)?;
                children.sort();
                pending.extend(children.into_iter().rev());
            } else {
                entries.push((relative, Some(std::fs::read(path).map_err(storage)?)));
            }
        }
        Ok(Self(entries))
    }

    pub fn restore(&self, root: &Path) -> Result<()> {
        // Parents precede children. No filesystem/runtime cache is shared:
        // every replay opens these independent provider bytes normally.
        for (relative, bytes) in &self.0 {
            let path = root.join(relative);
            match bytes {
                Some(bytes) => std::fs::write(path, bytes).map_err(storage)?,
                None => std::fs::create_dir(path).map_err(storage)?,
            }
        }
        Ok(())
    }
}
