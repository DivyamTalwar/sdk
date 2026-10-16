use acyclic_harness::{
    Result,
    model::{ModelContentPart, ModelRequest},
};
use std::borrow::Cow;

// ANT-TOOL-02, OAR-TOOL-03, OAC-TOOL-04, N-05. Reserve the alias
// namespace so an already valid ID cannot collide with another ID's alias.
const PREFIX: &str = "harness_call_";

pub(crate) struct CallIds<'a>(Vec<&'a str>);

fn ids(request: &ModelRequest) -> impl Iterator<Item = &str> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.parts())
        .filter_map(|part| match part {
            ModelContentPart::ToolCall(call) => Some(call.call_id.as_str()),
            ModelContentPart::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
}

fn unchanged(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && !id.starts_with(PREFIX)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

impl<'a> CallIds<'a> {
    pub(crate) fn new(request: &'a ModelRequest) -> Self {
        let count = ids(request).filter(|id| !unchanged(id)).count();
        if count == 0 {
            return Self(Vec::new());
        }
        let mut values = Vec::with_capacity(count);
        values.extend(ids(request).filter(|id| !unchanged(id)));
        values.sort_unstable();
        values.dedup();
        Self(values)
    }

    pub(crate) fn wire<'id>(&self, id: &'id str) -> Result<Cow<'id, str>> {
        if unchanged(id) {
            return Ok(Cow::Borrowed(id));
        }
        let index = self
            .0
            .binary_search(&id)
            .map_err(|_| crate::invalid("missing tool call ID binding"))?;
        Ok(Cow::Owned(format!("{PREFIX}{index}")))
    }

    #[cfg(all(not(target_arch = "wasm32"), any(test, feature = "fuzzing")))]
    pub(crate) fn assert_round_trip(request: &'a ModelRequest) {
        let table = Self::new(request);
        let mut wires = std::collections::BTreeMap::new();
        for id in ids(request) {
            let wire = table.wire(id).unwrap();
            assert!(!wire.is_empty() && wire.len() <= 40);
            assert!(
                wire.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            );
            if let Some(previous) = wires.insert(wire.to_string(), id) {
                assert_eq!(previous, id);
            }
            let restored = if let Some(index) = wire.strip_prefix(PREFIX) {
                table.0[index.parse::<usize>().unwrap()]
            } else {
                wire.as_ref()
            };
            assert_eq!(restored, id);
        }
    }
}
