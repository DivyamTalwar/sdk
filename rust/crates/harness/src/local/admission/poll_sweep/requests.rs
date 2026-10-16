//! Verify the retained request bound to every root-model effect, including orphans.
use super::*;
use crate::{
    executor::ExecutionJournal,
    model::{ModelRequest, ModelRole, PreparedModelRequest},
};

pub(super) async fn check(harness: &Harness, fixture: &Fixture, root: TaskId) -> Result<()> {
    let dispatches = fixture.provider.dispatches();
    let turn = harness
        .current_turn()
        .await?
        .0
        .expect("retained root turn")
        .root;
    let cancellation = acyclic_fs::CancellationToken::new();
    let work = acyclic_fs::NativeMaterializationWork::new(
        acyclic_fs::performance::work_budget(65_536, 64 * 1_024 * 1_024),
        &cancellation,
    )
    .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
    let (journal, operation) = harness
        .runtime
        .readonly_execution_journal_with_native_work(root, turn, &work)
        .await?;
    let mut checked = 0;
    for dispatch in dispatches
        .iter()
        .filter(|dispatch| dispatch.operation_id == operation)
    {
        let key = format!("model:{}:request", dispatch.step);
        let (request, _custody) = journal
            .staged_ref_with_native_work(operation, &key, &work)?
            .await?;
        let bytes = journal.load(&request, None)?.await?;
        let prepared = PreparedModelRequest::decode(&bytes, harness.limits, None)?;
        check_prompt(prepared.request())?;
        assert_eq!(
            prepared.request_digest(),
            dispatch.request_digest,
            "retained prompt differs from the actual generated/reconciled reservation"
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        match fixture.scenario {
            Scenario::Text => 1,
            Scenario::EffectAndContinuation => 2,
        }
    );
    Ok(())
}

fn check_prompt(request: &ModelRequest) -> Result<()> {
    if request.messages.iter().any(|message| {
        message.role == ModelRole::User
            && matches!(&message.content, ModelContent::Text(text) if text == PROMPT)
    }) {
        Ok(())
    } else {
        Err(Error::Invalid(
            "root model request lost the original prompt".into(),
        ))
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn request_check_rejects_replaced_and_missing_prompts(replacement in "[a-z]{0,40}") {
        let model = Model::new("scripted".into(), "test".into(), "1".into(), serde_json::Value::Null).unwrap();
        let mut request = ModelRequest {
            model,
            messages: vec![crate::model::ModelMessage { role: ModelRole::User, content: ModelContent::Text(PROMPT.into()) }],
            tools: vec![],
            max_output_tokens: None,
        };
        proptest::prop_assert!(check_prompt(&request).is_ok());
        request.messages[0].content = ModelContent::Text(replacement);
        proptest::prop_assert!(check_prompt(&request).is_err());
        request.messages.clear();
        proptest::prop_assert!(check_prompt(&request).is_err());
        // A scripted answer alone still succeeds in both broken variants; the
        // real sweep's independent request check must reject them.
    }
}
