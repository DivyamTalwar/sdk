use super::*;

impl<P: StreamProvider + ?Sized> CommandSession<P> {
    /// Whether an observed request for this session or selected task lacks its
    /// immutable outcome. Taking a request or losing a dispatch does not clear it.
    pub fn pending_execution(&self) -> Result<bool> {
        let selected = self.selected_task()?.map(|task| task.to_string());
        Ok(self.files.keys().any(|key| {
            let own = match key {
                files::FileKey::Session {
                    session,
                    file: files::TaskFile::Request(_),
                } => session.as_str() == self.session.to_string(),
                files::FileKey::Task {
                    task,
                    file: files::TaskFile::Request(_),
                } => selected.as_deref() == Some(task.as_str()),
                _ => false,
            };
            own && related(key, files::TaskFile::ExecutionResult)
                .is_some_and(|outcome| !self.files.contains_key(&outcome))
        }))
    }
}
