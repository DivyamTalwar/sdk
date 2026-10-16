use super::{AuthorityVerifier, Scope};
use crate::{Error, Result};
use acyclic_fs::access::Capability;

impl AuthorityVerifier {
    /// Verify a signed scope and extract its single filesystem grant payload.
    /// Call once when binding a view, then use `acyclic_fs::access::PathTrie` for
    /// checks. Principal attributes and operation context come from the host.
    pub fn filesystem_capability(&self, scope: &Scope) -> Result<Capability> {
        self.verify(scope, &mut None)?;
        let label = filesystem_scope_label(scope.capabilities(), &mut None)?
            .ok_or_else(|| Error::Unauthorized("scope has no filesystem grant".into()))?;
        let capability = Capability::from_scope_label(label)
            .map_err(|error| Error::Unauthorized(error.to_string()))?
            .ok_or_else(|| Error::Unauthorized("scope has no filesystem grant".into()))?;
        capability
            .validate()
            .map_err(|error| Error::Unauthorized(error.to_string()))?;
        Ok(capability)
    }
}

pub(crate) fn filesystem_scope_label<'a>(
    capabilities: &'a crate::Capabilities,
    work: &mut Option<&mut dyn crate::store::history::HistoryIo>,
) -> Result<Option<&'a str>> {
    let mut result = None;
    for label in capabilities.iter() {
        if let Some(io) = work.as_deref_mut() {
            io.check_cancellation()?;
            io.codec_charge(crate::executor::json::JsonCodecCharge::Inspect { items: 1 })?;
        }
        if label.starts_with(acyclic_fs::access::SCOPE_PREFIX) {
            if result.is_some() {
                return Err(crate::fork::boundary_error(
                    work,
                    crate::wire::ErrorCode::Unauthorized,
                    "scope has multiple filesystem grants",
                ));
            }
            result = Some(label);
        }
    }
    Ok(result)
}
