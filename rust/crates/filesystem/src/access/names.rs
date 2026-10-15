use super::InvalidPolicy;
use crate::model::{CaseSensitivity, UnicodePolicy, VolumeConfig};
use std::borrow::Cow;

// UTF-8 access paths use the same whole-name lowercase keys as LogicalName.
// RequireNfc admits NFC names; it does not equate distinct preserved spellings.
pub(super) fn key(path: &str, config: VolumeConfig) -> Result<Cow<'_, str>, InvalidPolicy> {
    if !super::glob::valid_path(path)
        || (config.unicode == UnicodePolicy::RequireNfc && !unicode_normalization::is_nfc(path))
        || path.len() > config.limits.maximum_path_bytes as usize
        || path
            .split('/')
            .any(|part| part.len() > config.limits.maximum_component_bytes as usize)
        || (!path.is_empty() && path.split('/').count() > config.limits.maximum_path_depth as usize)
    {
        return Err(InvalidPolicy("path violates volume name semantics"));
    }
    Ok(fold(path, config))
}

pub(super) fn fold(text: &str, config: VolumeConfig) -> Cow<'_, str> {
    if config.case_sensitivity == CaseSensitivity::ProfileFolded {
        Cow::Owned(text.to_lowercase())
    } else {
        Cow::Borrowed(text)
    }
}
