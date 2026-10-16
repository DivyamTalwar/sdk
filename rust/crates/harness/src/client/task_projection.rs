//! Presentation of task text files; bytes are combined before UTF-8 projection.
use super::{ClientViews, FileQueryView, TranscriptView};
use crate::task_volume::{Channel, FileKey, Query, TaskFile, ToolFile};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn project(views: &mut ClientViews, files: &FileQueryView) {
    if !matches!(files.query(), Query::Tasks) {
        return;
    }
    let mut text = BTreeMap::<(String, u64, bool), Vec<u8>>::new();
    let streamed: BTreeSet<_> = files
        .files()
        .keys()
        .filter_map(|key| match key {
            FileKey::Task {
                task,
                file: TaskFile::Chunk { .. },
            } => Some(task.clone()),
            _ => None,
        })
        .collect();
    let mut output = BTreeMap::<(crate::task_volume::Component, String), Vec<u8>>::new();
    for file in files.files().values() {
        let FileKey::Task { task, file: kind } = &file.key else {
            continue;
        };
        let owner = format!("AGGREGATE_KIND_TASK:{}", task.as_str());
        match kind {
            TaskFile::Chunk { turn, .. } => {
                if let Some(channel) = file.channel {
                    text.entry((owner, *turn, channel == Channel::Reasoning))
                        .or_default()
                        .extend_from_slice(&file.bytes);
                }
            }
            TaskFile::Tool {
                call,
                file: ToolFile::Stdout(_) | ToolFile::Stderr(_),
                ..
            } => {
                output
                    .entry((task.clone(), call.as_str().to_owned()))
                    .or_default()
                    .extend_from_slice(&file.bytes);
            }
            TaskFile::ExecutionResult(_) if !streamed.contains(task) => {
                if let Some((sequence, completed)) = super::command_output::text(file) {
                    text.insert((owner, sequence, false), completed.into_bytes());
                }
            }
            // TODO(#645): fold host-published request/status/completion schemas
            // when task-file execution admission publishes these records.
            _ => {}
        }
    }
    let owners: BTreeSet<_> = views
        .queries
        .get(&(files.cursor().volume, files.cursor().query))
        .into_iter()
        .chain(std::iter::once(files))
        .flat_map(|query| query.files().values())
        .filter_map(|file| match &file.key {
            FileKey::Task {
                task,
                file:
                    TaskFile::Chunk { .. }
                    | TaskFile::Tool {
                        file: ToolFile::Stdout(_) | ToolFile::Stderr(_),
                        ..
                    },
            } => Some(format!("AGGREGATE_KIND_TASK:{}", task.as_str())),
            FileKey::Task {
                task,
                file: TaskFile::ExecutionResult(_),
            } if super::command_output::text(file).is_some() => {
                Some(format!("AGGREGATE_KIND_TASK:{}", task.as_str()))
            }
            _ => None,
        })
        .collect();
    for owner in owners {
        views.transcripts.remove(&owner);
        views.tools.remove(&owner);
    }
    for ((owner, turn, reasoning), bytes) in text {
        let transcript = views
            .transcripts
            .entry(owner)
            .or_insert_with(TranscriptView::default);
        let target = if reasoning {
            &mut transcript.reasoning
        } else {
            &mut transcript.streaming_text
        };
        if !target.is_empty() {
            target.push_str("\n\n");
        }
        target.push_str(&String::from_utf8_lossy(&bytes));
        transcript.streaming_turn = Some(crate::OperationId::from_bytes(
            u128::from(turn).to_be_bytes(),
        ));
    }
    for ((task, call), bytes) in output {
        let authority = crate::wire::Authority {
            kind: crate::wire::AggregateKind::Task as i32,
            id: task.as_str().to_owned(),
        };
        views.tool_output(&authority, &call, &String::from_utf8_lossy(&bytes));
    }
}
