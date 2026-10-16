use acyclic_harness::{
    Result,
    model::{ModelContentPart, ModelEvent, ModelRequest},
};
use std::borrow::Cow;

// N-05, ANT-TOOL-01, OAR-TOOL-01, OAC-TOOL-03, GEM-TOOL-01.
// Reserve the namespace, including canonical names that already use it.
const PREFIX: &str = "harness_tool_";

pub(crate) struct Names<'a>(Vec<&'a str>);

fn names(request: &ModelRequest) -> impl Iterator<Item = &str> {
    request.tools.iter().map(|tool| tool.name.as_str()).chain(
        request
            .messages
            .iter()
            .flat_map(|message| message.content.parts())
            .filter_map(|part| match part {
                ModelContentPart::ToolCall(call) => Some(call.name.as_str()),
                ModelContentPart::ToolResult { name, .. } => Some(name.as_str()),
                _ => None,
            }),
    )
}

fn unchanged(name: &str) -> bool {
    !name.starts_with(PREFIX)
        && !name.is_empty()
        && name.len() <= 64
        && (name.as_bytes()[0].is_ascii_alphabetic() || name.starts_with('_'))
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

impl<'a> Names<'a> {
    #[cfg(any(test, feature = "fuzzing"))]
    pub(crate) fn assert_round_trip(request: &'a ModelRequest) {
        let table = Self::new(request);
        let mut seen = std::collections::BTreeSet::new();
        for &name in &table.0 {
            let wire = table.wire(name).unwrap();
            assert!(!wire.is_empty() && wire.len() <= 64);
            assert!(wire.as_bytes()[0].is_ascii_alphabetic() || wire.starts_with('_'));
            assert!(
                wire.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            );
            assert!(
                seen.insert(wire.to_string()),
                "distinct canonical names collided"
            );
            let mut event = ModelEvent::ToolCall(acyclic_harness::model::ToolCall::new(
                "call".into(),
                wire.into_owned(),
                serde_json::json!({}),
            ));
            table.restore(&mut event).unwrap();
            let ModelEvent::ToolCall(call) = event else {
                unreachable!()
            };
            assert_eq!(call.name, name);
            let _ = table.canonical(name); // Also exercise malformed/untrusted alias strings.
        }
    }

    pub(crate) fn new(request: &'a ModelRequest) -> Self {
        let mut values = Vec::with_capacity(names(request).count());
        values.extend(names(request));
        values.sort_unstable();
        values.dedup();
        Self(values)
    }

    pub(crate) fn wire<'n>(&self, name: &'n str) -> Result<Cow<'n, str>> {
        if unchanged(name) {
            return Ok(Cow::Borrowed(name));
        }
        let index = self
            .0
            .binary_search(&name)
            .map_err(|_| crate::invalid("missing tool name binding"))?;
        Ok(Cow::Owned(format!("{PREFIX}{index}")))
    }

    fn canonical(&self, wire: &str) -> Result<Option<&'a str>> {
        let Some(index) = wire.strip_prefix(PREFIX) else {
            return Ok(None);
        };
        if index.is_empty()
            || !index.bytes().all(|byte| byte.is_ascii_digit())
            || (index.len() > 1 && index.starts_with('0'))
        {
            return Err(crate::invalid("invalid model tool alias"));
        }
        let name = index
            .parse::<usize>()
            .ok()
            .and_then(|index| self.0.get(index))
            .copied()
            .ok_or_else(|| crate::invalid("unknown model tool alias"))?;
        if unchanged(name) {
            return Err(crate::invalid("invalid model tool alias"));
        }
        Ok(Some(name))
    }

    pub(crate) fn restore(&self, event: &mut ModelEvent) -> Result<()> {
        if let ModelEvent::ToolCall(call) = event {
            if let Some(name) = self.canonical(&call.name)? {
                call.name = name.into();
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn native(
        request: &'a ModelRequest,
        work: &acyclic_fs::NativeMaterializationWork,
    ) -> Result<(Self, acyclic_fs::NativeAllocation)> {
        let count = names(request).count() as u64;
        let bytes = count
            .checked_mul(std::mem::size_of::<&str>() as u64)
            .ok_or_else(|| crate::invalid("tool name binding size overflow"))?;
        let custody = work.claim(
            bytes,
            u64::from(count != 0),
            acyclic_fs::WorkCounters {
                bytes_copied: bytes,
                items_examined: count,
                ..acyclic_fs::WorkCounters::UNCHARGED
            },
        )?;
        Ok((Self::new(request), custody))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn restore_native(
        &self,
        event: &mut ModelEvent,
        allocation: &mut acyclic_fs::NativeAllocation,
        work: &acyclic_fs::NativeMaterializationWork,
    ) -> Result<()> {
        if let ModelEvent::ToolCall(call) = event {
            if let Some(name) = self.canonical(&call.name)? {
                let mut custody = work.claim(
                    name.len() as u64,
                    1,
                    acyclic_fs::WorkCounters {
                        bytes_copied: name.len() as u64,
                        ..acyclic_fs::WorkCounters::UNCHARGED
                    },
                )?;
                allocation.absorb(&mut custody)?;
                call.name = name.into();
            }
        }
        Ok(())
    }
}
