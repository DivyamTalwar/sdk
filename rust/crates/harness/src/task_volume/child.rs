//! Child lifecycle intents stored in the parent's task volume.
use super::{Component, Error, Result};
use serde::{Deserialize, Serialize};

/// Files owned by a parent below `agents/<ref>/`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum ChildFile {
    /// Immutable spawn prompt with optional model and tool frontmatter.
    Request,
    /// Immutable message, identified by the sender's operation identity.
    Inbox(Component),
    /// Latest lifecycle intent. Publication is fenced by the task-volume cut.
    Control,
}

impl ChildFile {
    pub(super) fn parse(parts: &[&str]) -> Result<Self> {
        match parts {
            ["request.md"] => Ok(Self::Request),
            ["inbox", name] => Ok(Self::Inbox(Component::new(
                name.strip_suffix(".md").ok_or(Error::InvalidPath)?,
            )?)),
            ["control"] => Ok(Self::Control),
            _ => Err(Error::InvalidPath),
        }
    }

    pub(super) fn path(&self) -> String {
        match self {
            Self::Request => "request.md".into(),
            Self::Inbox(id) => format!("inbox/{}.md", id.as_str()),
            Self::Control => "control".into(),
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

    proptest! {
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
