use super::*;
use crate::access::{AccessView, Action, PinnedPath};

pub(in crate::native_mount) fn access_time() -> Result<u64, MountSourceError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| MountSourceError::AccessDenied)
}

pub(in crate::native_mount) fn require(
    path: &PinnedPath,
    action: Action,
    now: u64,
) -> Result<(), MountSourceError> {
    if path.decide(action, now).allowed {
        return Ok(());
    }
    let mode = path.mount_mode(now);
    if matches!(
        action,
        Action::Create | Action::Write | Action::Append | Action::Delete | Action::Rename
    ) && mode.read_only()
        && (mode.visible() || mode.allows(Action::Read))
    {
        Err(MountSourceError::ReadOnly)
    } else {
        Err(MountSourceError::AccessDenied)
    }
}

pub(in crate::native_mount) fn require_open(
    path: &PinnedPath,
    access: Option<MountOpenAccess>,
) -> Result<(), MountSourceError> {
    let now = access_time()?;
    if let Some(access) = access {
        require(path, Action::List, now)?;
        if matches!(access, MountOpenAccess::Read | MountOpenAccess::ReadWrite) {
            require(path, Action::Read, now)?;
        }
        if matches!(access, MountOpenAccess::Write | MountOpenAccess::ReadWrite)
            && !path.decide(Action::Append, now).allowed
        {
            require(path, Action::Write, now)?;
        }
        Ok(())
    } else if [Action::List, Action::Read, Action::Write, Action::Append]
        .into_iter()
        .any(|action| path.decide(action, now).allowed)
    {
        Ok(())
    } else {
        Err(MountSourceError::AccessDenied)
    }
}

impl<A, O> CheckoutMountSource<A, O> {
    /// Bind the authenticated policy/capability view used by every callback.
    pub fn with_access_view(mut self, view: Arc<AccessView>) -> Result<Self, NativeMountError> {
        let current = view.current().map_err(|_| MountSourceError::AccessDenied)?;
        if current.config() != self.checkout.config
            || uuid::Uuid::try_parse(current.volume())
                .map(|id| id.into_bytes())
                .ok()
                != Some(self.checkout.volume_id.into_bytes())
        {
            return Err(MountSourceError::AccessDenied.into());
        }
        drop(current);
        self.access_view = Some(view);
        Ok(self)
    }

    pub(super) fn require_paths(
        &self,
        requests: &[(Action, &MountPath)],
        work: &crate::NativeMaterializationWork,
    ) -> Result<(), MountSourceError> {
        let view = self.access_snapshot()?;
        let now = access_time()?;
        let mut carrier = NativeDemandCarrier::Native(work);
        let mut inputs = InputBuffers::new(&mut carrier)?;
        for (action, path) in requests {
            inputs.items(1)?;
            let path = self.namespace_inputs(path, &mut inputs)?;
            let permissions = view
                .pin(&path)
                .map_err(|_| MountSourceError::AccessDenied)?;
            require(&permissions, *action, now)?;
        }
        Ok(())
    }

    pub(in crate::native_mount) fn permissions_for_path(
        &self,
        path: &MountPath,
        work: &crate::NativeMaterializationWork,
    ) -> Result<PinnedPath, MountSourceError> {
        let mut carrier = NativeDemandCarrier::Native(work);
        let mut inputs = InputBuffers::new(&mut carrier)?;
        let path = self.namespace_inputs(path, &mut inputs)?;
        self.pin_access(&path)
    }

    pub(in crate::native_mount) fn access_snapshot(
        &self,
    ) -> Result<Arc<crate::access::PathTrie>, MountSourceError> {
        self.access_view
            .as_ref()
            .ok_or(MountSourceError::AccessDenied)?
            .current()
            .map_err(|_| MountSourceError::AccessDenied)
    }

    pub(in crate::native_mount) fn pin_access(
        &self,
        path: &NamespacePath,
    ) -> Result<PinnedPath, MountSourceError> {
        self.access_snapshot()?
            .pin(path)
            .map_err(|_| MountSourceError::AccessDenied)
    }

    pub(super) fn require_access(
        &self,
        path: &NamespacePath,
        action: Action,
    ) -> Result<PinnedPath, MountSourceError> {
        let pin = self.pin_access(path)?;
        require(&pin, action, access_time()?)?;
        Ok(pin)
    }
}
