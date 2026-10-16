//! Child lifecycle intents stored in the parent's task volume.
use super::{Component, Error, Result};
use serde::{Deserialize, Serialize};

/// Spawn request read from `agents/<ref>/request.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildRequest {
    /// Markdown prompt, excluding the optional frontmatter.
    pub prompt: String,
    /// Model name within the parent's configured provider.
    pub model: Option<String>,
    /// Requested subset of the parent's tool grants.
    pub tools: Option<Vec<String>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestMetadata {
    model: Option<String>,
    tools: Option<Vec<String>>,
}

impl ChildRequest {
    /// Parses a bounded Markdown request with flat YAML metadata. Aliases,
    /// duplicate keys, tags and recursive metadata cannot grant authority.
    pub fn parse(bytes: &[u8], maximum_bytes: u64) -> crate::Result<Self> {
        let invalid = || crate::Error::Invalid("invalid child request".into());
        if bytes.is_empty() || bytes.len() as u64 > maximum_bytes {
            return Err(invalid());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let (metadata, prompt) = if text.starts_with("---\n") || text.starts_with("---\r\n") {
            let (start, end, body) = crate::context::frontmatter_end(bytes)?;
            let header = text.get(start..end).ok_or_else(invalid)?;
            let options = serde_saphyr::options! {
                budget: serde_saphyr::budget! {
                    max_events: header.len().saturating_add(1),
                    max_nodes: header.len().saturating_add(1), max_depth: 3,
                    flow_nesting_limit: 3, max_documents: 1,
                    max_aliases: 0, max_anchors: 0, max_merge_keys: 0,
                    max_total_scalar_bytes: header.len(),
                },
                duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
                merge_keys: serde_saphyr::MergeKeyPolicy::Error,
                reject_unsupported_tags: true, emit_comments: false, with_snippet: false,
            };
            let metadata = if header.trim().is_empty() {
                RequestMetadata::default()
            } else {
                serde_saphyr::from_str_with_options(header, options).map_err(|_| invalid())?
            };
            (metadata, text.get(body..).ok_or_else(invalid)?)
        } else {
            (RequestMetadata::default(), text)
        };
        if prompt.is_empty()
            || metadata.model.as_ref().is_some_and(String::is_empty)
            || metadata
                .tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(String::is_empty))
        {
            return Err(invalid());
        }
        Ok(Self {
            prompt: prompt.into(),
            model: metadata.model,
            tools: metadata.tools,
        })
    }

    /// Renders metadata as JSON within YAML frontmatter, preserving the literal
    /// prompt even when that prompt itself begins with a frontmatter delimiter.
    pub fn encode(&self) -> Vec<u8> {
        if self.model.is_some()
            || self.tools.is_some()
            || self.prompt.starts_with("---\n")
            || self.prompt.starts_with("---\r\n")
        {
            format!(
                "---\n{}\n---\n{}",
                serde_json::json!({
                    "model":self.model,"tools":self.tools,
                }),
                self.prompt
            )
            .into_bytes()
        } else {
            self.prompt.as_bytes().to_vec()
        }
    }
}

/// Files owned by a parent below `agents/<ref>/`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum ChildFile {
    /// Immutable spawn prompt with optional model and tool frontmatter.
    Request,
    /// Immutable message, identified by the sender's operation identity.
    Inbox(Component),
    /// Latest lifecycle intent. Publication is fenced by the task-volume cut.
    Control,
    /// Harness-owned execution status, followed by the parent and UI.
    Status,
}

impl ChildFile {
    pub(super) fn parse(parts: &[&str]) -> Result<Self> {
        match parts {
            ["request.md"] => Ok(Self::Request),
            ["inbox", name] => Ok(Self::Inbox(Component::new(
                name.strip_suffix(".md").ok_or(Error::InvalidPath)?,
            )?)),
            ["control"] => Ok(Self::Control),
            ["status.json"] => Ok(Self::Status),
            _ => Err(Error::InvalidPath),
        }
    }

    pub(super) fn path(&self) -> String {
        match self {
            Self::Request => "request.md".into(),
            Self::Inbox(id) => format!("inbox/{}.md", id.as_str()),
            Self::Control => "control".into(),
            Self::Status => "status.json".into(),
        }
    }
}

/// Exact text accepted in a child's control file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Control {
    /// Stop starting work until resumed.
    Pause,
    /// Continue paused or interrupted work.
    Resume,
    /// Stop the current turn, retaining the child for further messages.
    Interrupt,
    /// Permanently cancel this child identity.
    Cancel,
    /// Permanently freeze this child identity, retaining its result view.
    Archive,
}

impl Control {
    /// Parses canonical file bytes; aliases and whitespace are rejected.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        match bytes {
            b"pause" => Ok(Self::Pause),
            b"resume" => Ok(Self::Resume),
            b"interrupt" => Ok(Self::Interrupt),
            b"cancel" => Ok(Self::Cancel),
            b"archive" => Ok(Self::Archive),
            _ => Err(Error::InvalidSegment),
        }
    }

    /// Canonical file content.
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Pause => b"pause",
            Self::Resume => b"resume",
            Self::Interrupt => b"interrupt",
            Self::Cancel => b"cancel",
            Self::Archive => b"archive",
        }
    }
}

/// Desired execution state, folded from accepted control-file publications.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ChildState {
    /// Child may start or continue work.
    #[default]
    Running,
    /// Child retains work but may not advance it.
    Paused,
    /// Current turn has been interrupted.
    Interrupted,
    /// Child has been cancelled permanently.
    Cancelled,
    /// Child is frozen permanently and omitted from active lists.
    Archived,
}

impl ChildState {
    /// Pure lifecycle reducer. Terminal identities cannot be reused or resumed.
    #[must_use]
    pub const fn apply(self, control: Control) -> Self {
        if matches!(self, Self::Cancelled | Self::Archived) {
            return self;
        }
        match control {
            Control::Pause => Self::Paused,
            Control::Resume => Self::Running,
            Control::Interrupt => Self::Interrupted,
            Control::Cancel => Self::Cancelled,
            Control::Archive => Self::Archived,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CONTROLS: [Control; 5] = [
        Control::Pause,
        Control::Resume,
        Control::Interrupt,
        Control::Cancel,
        Control::Archive,
    ];

    #[test]
    fn every_transition_is_idempotent_and_terminal_states_are_absorbing() {
        // Exhaust the complete bounded reducer domain rather than sample it.
        for state in [
            ChildState::Running,
            ChildState::Paused,
            ChildState::Interrupted,
            ChildState::Cancelled,
            ChildState::Archived,
        ] {
            for control in CONTROLS {
                let next = state.apply(control);
                assert_eq!(next.apply(control), next);
                if matches!(state, ChildState::Cancelled | ChildState::Archived) {
                    assert_eq!(next, state);
                }
                assert_eq!(Control::parse(control.bytes()).unwrap(), control);
            }
        }
    }

    #[test]
    fn request_metadata_rejects_aliases_duplicates_and_recursive_shapes() {
        for header in [
            "model: first\nmodel: second",
            "tools: &tools [read]\nmodel: *tools",
            "tools: [[read]]",
            "tools: [read]\ncapabilities: [admin]",
            "model: ''",
            "tools: ['']",
        ] {
            let request = format!("---\n{header}\n---\nwork");
            assert!(
                ChildRequest::parse(request.as_bytes(), 4096).is_err(),
                "{header}"
            );
        }
        for prompt in ["---\nliteral prompt", "---\r\nliteral prompt"] {
            let request = ChildRequest {
                prompt: prompt.into(),
                model: None,
                tools: None,
            };
            assert_eq!(
                ChildRequest::parse(&request.encode(), 4096).unwrap(),
                request
            );
        }
    }

    proptest! {
        #[test]
        fn request_metadata_round_trips_without_changing_the_literal_prompt(
            prompt in ".{1,100}", model in prop::option::of("[a-z]{1,10}"),
            tools in prop::option::of(prop::collection::vec("[a-z]{1,10}", 0..8)),
        ) {
            let request = ChildRequest { prompt, model, tools };
            let encoded = request.encode();
            prop_assert_eq!(ChildRequest::parse(&encoded, encoded.len() as u64).unwrap(), request);
            prop_assert!(ChildRequest::parse(&encoded, encoded.len() as u64 - 1).is_err());
        }

        #[test]
        fn terminal_state_survives_every_suffix(commands in prop::collection::vec(0usize..5, 0..100)) {
            for terminal in [ChildState::Archived, ChildState::Cancelled] {
                let state = commands.iter().fold(terminal, |state, &i| state.apply(CONTROLS[i]));
                prop_assert_eq!(state, terminal);
            }
        }

        #[test]
        fn accepted_control_bytes_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..32)) {
            if let Ok(control) = Control::parse(&bytes) {
                prop_assert_eq!(control.bytes(), bytes.as_slice());
            }
        }

        #[test]
        fn child_paths_round_trip_and_queries_select_exactly_the_child(
            reference in "[a-z][a-z0-9_-]{0,20}", message in "[a-z][a-z0-9_-]{0,20}",
        ) {
            use super::super::{FileKey, Query};
            let id = Component::new(&reference).unwrap();
            for file in [ChildFile::Request, ChildFile::Control, ChildFile::Inbox(Component::new(message).unwrap())] {
                let key = FileKey::Child { reference: id.clone(), file };
                prop_assert_eq!(FileKey::parse(&key.path()).unwrap(), key.clone());
                prop_assert!(Query::Children.matches(&key));
                prop_assert!(Query::Child(id.clone()).matches(&key));
                prop_assert!(!Query::Child(Component::new(format!("other-{reference}")).unwrap()).matches(&key));
            }
        }
    }
}
