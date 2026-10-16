use super::{
    Channel, ChildFile, Component, Error, FileKey, InputPath, PlanName, Result, TaskFile, ToolFile,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Folder selectors have bounded matching cost and allocate no per-chunk paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// Entire private task volume.
    Root,
    /// Command definitions and workspace files used by the input UI.
    Inputs,
    /// Declarative UI settings, keymap and theme files.
    Config,
    /// Editable Markdown files in the private volume's plans folder.
    Plans,
    /// All task records.
    Tasks,
    /// One task subtree.
    Task(Component),
    /// Immutable task requests.
    Requests(Component),
    /// All model turns and their tool subtrees.
    Turns(Component),
    /// One model turn, including its tools.
    Turn {
        /// Task identity.
        task: Component,
        /// Turn/attempt identity.
        turn: u64,
    },
    /// Assistant chunks in numeric sequence order in a snapshot.
    Chunks {
        /// Task identity.
        task: Component,
        /// Turn/attempt identity.
        turn: u64,
    },
    /// Tool invocations in one turn.
    Tools {
        /// Task identity.
        task: Component,
        /// Turn/attempt identity.
        turn: u64,
    },
    /// One invocation subtree.
    Tool {
        /// Task identity.
        task: Component,
        /// Turn/attempt identity.
        turn: u64,
        /// Provider tool call identity.
        call: Component,
    },
    /// Approval requests and decisions.
    Approvals(Component),
    /// Notifications for one task.
    Notifications(Component),
    /// Child mount declarations.
    Agents,
    /// All child requests, inbox files and lifecycle intents.
    Children,
    /// One child subtree.
    Child(Component),
    /// One exact logical file, including goal, plan, status and mount declarations.
    File(FileKey),
}

impl Query {
    pub(super) fn ranges(&self) -> Vec<(std::ops::Bound<FileKey>, std::ops::Bound<FileKey>)> {
        use std::ops::Bound::{Excluded, Included, Unbounded};
        let range = |task: &Component, first: TaskFile, last: TaskFile| {
            let bound = |file| {
                Included(FileKey::Task {
                    task: task.clone(),
                    file,
                })
            };
            (bound(first), bound(last))
        };
        let minimum = Component(Arc::from(""));
        let maximum = Component(Arc::from("\u{10ffff}"));
        let chunks = |task: &Component, turn: u64| {
            range(
                task,
                TaskFile::Chunk { turn, sequence: 0 },
                TaskFile::Chunk {
                    turn,
                    sequence: u64::MAX,
                },
            )
        };
        let tools = |task: &Component, turn: u64, first: &Component, last: &Component| {
            range(
                task,
                TaskFile::Tool {
                    turn,
                    call: first.clone(),
                    file: ToolFile::Request,
                },
                TaskFile::Tool {
                    turn,
                    call: last.clone(),
                    file: ToolFile::Result,
                },
            )
        };
        vec![match self {
            Self::Config => (
                Included(FileKey::Input(InputPath("config/".into()))),
                Included(FileKey::Input(InputPath("config/\u{10ffff}".into()))),
            ),
            Self::Inputs => (
                Included(FileKey::Input(InputPath(String::new()))),
                Included(FileKey::Input(InputPath("\u{10ffff}".into()))),
            ),
            Self::Plans => (
                Included(FileKey::Plan(PlanName(Arc::from("")))),
                Excluded(FileKey::Agent(minimum)),
            ),
            Self::Children => (
                Included(FileKey::Child {
                    reference: minimum,
                    file: ChildFile::Request,
                }),
                Included(FileKey::Child {
                    reference: maximum,
                    file: ChildFile::Status,
                }),
            ),
            Self::Child(reference) => (
                Included(FileKey::Child {
                    reference: reference.clone(),
                    file: ChildFile::Request,
                }),
                Included(FileKey::Child {
                    reference: reference.clone(),
                    file: ChildFile::Status,
                }),
            ),
            Self::Agents => (
                Included(FileKey::Agent(minimum)),
                Included(FileKey::Agent(maximum)),
            ),
            Self::Turn { task, turn } => {
                return vec![
                    range(
                        task,
                        TaskFile::TurnRequest(*turn),
                        TaskFile::TurnRequest(*turn),
                    ),
                    chunks(task, *turn),
                    range(
                        task,
                        TaskFile::Completion(*turn),
                        TaskFile::Completion(*turn),
                    ),
                    tools(task, *turn, &minimum, &maximum),
                ];
            }
            Self::Chunks { task, turn }
            | Self::File(FileKey::Task {
                task,
                file: TaskFile::TextHead { turn, .. },
            }) => chunks(task, *turn),
            Self::Tools { task, turn } => tools(task, *turn, &minimum, &maximum),
            Self::Tool { task, turn, call } => tools(task, *turn, call, call),
            Self::Requests(task) => {
                range(task, TaskFile::Request(minimum), TaskFile::Request(maximum))
            }
            Self::Turns(task) => range(
                task,
                TaskFile::TurnRequest(0),
                TaskFile::Tool {
                    turn: u64::MAX,
                    call: maximum,
                    file: ToolFile::Result,
                },
            ),
            Self::Approvals(task) => range(
                task,
                TaskFile::ApprovalRequest(minimum),
                TaskFile::ApprovalDecision {
                    request: maximum.clone(),
                    principal: maximum,
                },
            ),
            Self::Notifications(task) => range(
                task,
                TaskFile::Notification(minimum),
                TaskFile::Notification(maximum),
            ),
            Self::Task(task) => range(task, TaskFile::Request(minimum), TaskFile::Context(maximum)),
            Self::File(key) => (Included(key.clone()), Included(key.clone())),
            _ => (Unbounded, Unbounded),
        }]
    }

    /// Matches typed keys without rendering paths or decoding payload bytes.
    pub fn matches(&self, key: &FileKey) -> bool {
        match (self, key) {
            (Self::Root, _)
            | (Self::Plans, FileKey::Plan(_))
            | (Self::Agents, FileKey::Agent(_))
            | (Self::Tasks, FileKey::Task { .. }) => true,
            (Self::Children, FileKey::Child { .. }) => true,
            (Self::Child(wanted), FileKey::Child { reference, .. }) => wanted == reference,
            (Self::Inputs, FileKey::Input(path)) => !path.0.starts_with("config/"),
            (Self::Config, FileKey::Input(path)) => matches!(
                path.0.as_str(),
                "config/settings.json" | "config/keymap.json" | "config/theme.json"
            ),
            (Self::Turns(t), FileKey::Task { task, file }) => t == task && file.turn().is_some(),
            (Self::Turn { task: t, turn }, FileKey::Task { task, file }) => {
                t == task && file.turn() == Some(*turn)
            }
            (
                Self::File(FileKey::Task {
                    task: t,
                    file: TaskFile::TextHead { turn, .. },
                }),
                FileKey::Task {
                    task,
                    file: TaskFile::Chunk { turn: n, .. },
                },
            )
            | (
                Self::Chunks { task: t, turn },
                FileKey::Task {
                    task,
                    file: TaskFile::Chunk { turn: n, .. },
                },
            )
            | (
                Self::Tools { task: t, turn },
                FileKey::Task {
                    task,
                    file: TaskFile::Tool { turn: n, .. },
                },
            ) => t == task && turn == n,
            (
                Self::Tool {
                    task: t,
                    turn,
                    call,
                },
                FileKey::Task {
                    task,
                    file:
                        TaskFile::Tool {
                            turn: n, call: c, ..
                        },
                },
            ) => t == task && turn == n && call == c,
            (Self::Task(t), FileKey::Task { task, .. })
            | (
                Self::Requests(t),
                FileKey::Task {
                    task,
                    file: TaskFile::Request(_),
                },
            )
            | (
                Self::Approvals(t),
                FileKey::Task {
                    task,
                    file: TaskFile::ApprovalRequest(_) | TaskFile::ApprovalDecision { .. },
                },
            )
            | (
                Self::Notifications(t),
                FileKey::Task {
                    task,
                    file: TaskFile::Notification(_),
                },
            ) => t == task,
            (Self::File(wanted), actual) => wanted == actual,
            _ => false,
        }
    }

    pub(super) fn matches_file(&self, file: &super::FileValue) -> bool {
        self.matches(&file.key)
            && match self {
                Self::File(FileKey::Task {
                    file: TaskFile::TextHead { channel, .. },
                    ..
                }) => file.channel == Some(*channel),
                _ => true,
            }
    }

    /// Resolves a canonical folder selection at the wire boundary.
    pub fn folder(path: &str) -> Result<Self> {
        if path.len() > 4096 {
            return Err(Error::InvalidPath);
        }
        let parts: Vec<_> = path.split('/').collect();
        let component = |s: &str| Component::new(s);
        let number = |s: &str| -> Result<u64> {
            let n = s.parse::<u64>().map_err(|_| Error::InvalidPath)?;
            if n.to_string() != s {
                return Err(Error::InvalidPath);
            }
            Ok(n)
        };
        Ok(match parts.as_slice() {
            [""] => Self::Root,
            ["plans"] => Self::Plans,
            ["agents"] => Self::Children,
            ["agents", reference] => Self::Child(component(reference)?),
            ["config"] => Self::Config,
            [".volumes", "agents"] => Self::Agents,
            [".tasks"] => Self::Tasks,
            [".tasks", t] => Self::Task(component(t)?),
            [".tasks", t, "requests"] => Self::Requests(component(t)?),
            [".tasks", t, "turns"] => Self::Turns(component(t)?),
            [".tasks", t, "approvals"] => Self::Approvals(component(t)?),
            [".tasks", t, "notifications"] => Self::Notifications(component(t)?),
            [".tasks", t, "turns", n] => Self::Turn {
                task: component(t)?,
                turn: number(n)?,
            },
            [".tasks", t, "turns", n, "chunks"] => Self::Chunks {
                task: component(t)?,
                turn: number(n)?,
            },
            [".tasks", t, "turns", n, "tools"] => Self::Tools {
                task: component(t)?,
                turn: number(n)?,
            },
            [".tasks", t, "turns", n, "tools", c] => Self::Tool {
                task: component(t)?,
                turn: number(n)?,
                call: component(c)?,
            },
            _ => Self::File(FileKey::parse(path)?),
        })
    }
}

#[cfg(test)]
#[path = "../../tests/task_volume/ranges.rs"]
mod tests;
