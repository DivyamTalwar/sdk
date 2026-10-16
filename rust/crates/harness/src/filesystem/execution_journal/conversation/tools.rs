use super::*;

impl<P, A, O> FilesystemExecutionJournal<P, A, O>
where
    P: StreamProvider + 'static,
    A: AsyncAuthorityStore + 'static,
    O: AsyncObjectStore + 'static,
{
    pub(super) async fn append_turn_history(
        &self,
        aggregate: &mut StreamAggregate<P>,
        operation_id: OperationId,
        user_id: Uuid,
        limits: Limits,
    ) -> Result<Value> {
        let mut usage = Value::Null;
        let mut calls = BTreeMap::new();
        let mut results = Vec::new();
        let mut current_step = None;
        let mut replay = crate::executor::ExecutionReplay::new(operation_id);
        loop {
            let page = self
                .replay_page(
                    operation_id,
                    replay.tail(),
                    crate::executor::EXECUTION_REPLAY_PAGE_RECORDS,
                    true,
                    None,
                )
                .await?;
            replay.observe_page(page.iter().map(|(record, _)| record), None)?;
            if page.is_empty() {
                break;
            }
            for (record, model) in page {
                if let Some(ModelEvent::Completed { metadata }) = model {
                    crate::model::accumulate_usage(&mut usage, metadata)?;
                }
                if let ExecutionEvent::ToolStarted { step, .. } = &record.event {
                    if current_step != Some(*step) {
                        for result in results.drain(..) {
                            self.append_tool_record(
                                aggregate,
                                operation_id,
                                user_id,
                                limits,
                                &mut calls,
                                result,
                            )
                            .await?;
                        }
                        calls.clear();
                        current_step = Some(*step);
                    }
                }
                if matches!(record.event, ExecutionEvent::ToolCompleted { .. }) {
                    results.push(record);
                } else {
                    self.append_tool_record(
                        aggregate,
                        operation_id,
                        user_id,
                        limits,
                        &mut calls,
                        record,
                    )
                    .await?;
                }
            }
        }
        for result in results {
            self.append_tool_record(aggregate, operation_id, user_id, limits, &mut calls, result)
                .await?;
        }
        Ok(usage)
    }

    async fn append_tool_record(
        &self,
        aggregate: &mut StreamAggregate<P>,
        operation_id: OperationId,
        user_id: Uuid,
        limits: Limits,
        calls: &mut BTreeMap<(u32, String), Uuid>,
        record: crate::executor::ExecutionRecord,
    ) -> Result<()> {
        let (kind, content, attachments, reply_to, call_id, step) = match record.event {
            ExecutionEvent::ToolStarted {
                step,
                call_id,
                invocation,
            } => (
                MessageKind::ToolCall,
                invocation,
                ReferencedAttachments::Inline { items: Vec::new() },
                Some(user_id),
                call_id,
                step,
            ),
            ExecutionEvent::ToolCompleted {
                step,
                call_id,
                result,
                projection,
            } => {
                let call = calls
                    .get(&(step, call_id.clone()))
                    .copied()
                    .ok_or_else(|| Error::Storage("tool result has no durable start".into()))?;
                (
                    MessageKind::ToolResult,
                    result,
                    ReferencedAttachments::Inline {
                        items: vec![Attachment {
                            file: projection,
                            label: Some("model_projection".into()),
                        }],
                    },
                    Some(call),
                    call_id,
                    step,
                )
            }
            _ => return Ok(()),
        };
        let publication = derived_operation_id(
            operation_id,
            format!("conversation-tool:{}", record.sequence).as_bytes(),
        );
        let id = Uuid::from_bytes(publication.into_bytes());
        if kind == MessageKind::ToolCall && calls.insert((step, call_id.clone()), id).is_some() {
            return Err(Error::Storage(
                "duplicate durable tool call identity".into(),
            ));
        }
        let existing = self
            .indexed_conversation_message(aggregate, id, limits)
            .await?;
        if let Some(existing) = existing {
            if existing.kind != kind
                || existing.content != content
                || existing.attachments != attachments
                || existing.reply_to != reply_to
                || existing.tool_call_id.as_deref() != Some(call_id.as_str())
            {
                return Err(Error::Conflict(
                    "tool publication identity belongs to another message".into(),
                ));
            }
            return Ok(());
        }
        let message = ConversationMessage {
            id,
            sequence: Self::next_conversation_sequence(aggregate)?,
            kind,
            content,
            attachments,
            reply_to,
            tool_call_id: Some(call_id),
            extensions: BTreeMap::new(),
        };
        self.append_conversation(
            aggregate,
            publication,
            "tool",
            Action::AppendConversationMessage {
                message: Box::new(message),
            },
        )
        .await?;
        Ok(())
    }
}
