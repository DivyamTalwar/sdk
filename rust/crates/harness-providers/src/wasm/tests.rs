use super::*;
use crate::{Adapter, fixtures::fixtures};
use acyclic_harness::model::{ModelContent, ModelMessage, ModelRequest, ModelRole};
use js_sys::Promise;
use serde_json::{Value, json};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function modelFixture(data, mode = 'complete') {
  let ready;
  const state = { calls: 0, aborted: false, ready: new Promise(resolve => ready = resolve) };
  state.fetch = (url, init) => {
    state.calls++;
    state.url = url;
    state.headers = new Headers(init.headers);
    state.body = JSON.parse(new TextDecoder().decode(init.body));
    init.signal.addEventListener('abort', () => { state.aborted = true; }, {once: true});
    ready();
    if (mode === 'pending') return new Promise((_, reject) => {
      init.signal.addEventListener('abort', () => reject(new Error('aborted')), {once: true});
    });
    const bytes = new TextEncoder().encode(data);
    return Promise.resolve(new Response(new ReadableStream({start(controller) {
      for (const byte of bytes) controller.enqueue(new Uint8Array([byte]));
      controller.close();
    }})));
  };
  return state;
}
export function modelContentFixture(data) {
  const state = modelFixture('data: [DONE]\n\n');
  state.reads = 0;
  state.reader = (file, maximum) => {
    state.reads++;
    state.file = file;
    state.maximum = maximum;
    return Promise.resolve(new Uint8Array(data));
  };
  return state;
}
export async function collectModel(iterator) {
  const events = [];
  for await (const event of iterator) events.push(event);
  return events;
}
export async function cancelModel(iterator, signal, ready, useReturn) {
  const pending = iterator.next();
  await ready;
  if (useReturn) await iterator.return(); else signal.abort();
  try {
    const result = await pending;
    if (!useReturn || !result.done) throw new Error('cancel did not terminate iterator');
  } catch (error) { if (useReturn || error.name !== 'AbortError') throw error; }
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = modelFixture)]
    fn model_fixture(data: &str, mode: &str) -> JsValue;
    #[wasm_bindgen(js_name = modelContentFixture)]
    fn content_fixture(data: &Uint8Array) -> JsValue;
    #[wasm_bindgen(js_name = collectModel)]
    fn collect_model(iterator: JsValue) -> Promise;
    #[wasm_bindgen(js_name = cancelModel)]
    fn cancel_model(
        iterator: JsValue,
        signal: &web_sys::AbortController,
        ready: JsValue,
        use_return: bool,
    ) -> Promise;
}

fn field(value: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    Reflect::get(value, &name.into())
}

fn provider(adapter: Adapter, fixture: &JsValue) -> Result<WasmModelProvider, JsValue> {
    let mut registry = WasmProviderRegistry::new(
        to_js(&json!({"fixture":"key"}))?,
        to_js(&Limits::default())?,
        Some(field(fixture, "fetch")?.dyn_into::<Function>()?),
        None,
    )?;
    registry.register(
        "fixture".into(),
        to_js(&json!({
            "adapter": adapter, "authentication":"api_key", "base_url":"https://example.test/v1",
            "models":{"model":{"context_tokens":4096,"output_tokens":512}},
            "max_event_bytes":4096,
        }))?,
    )?;
    registry.resolve("fixture/model", None)
}

fn request(
    provider: &WasmModelProvider,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<JsValue, JsValue> {
    let model = provider.provider.model().clone();
    let prepared = PreparedModelRequest::prepare(
        ModelRequest {
            model,
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: ModelContent::Text("hello".into()),
            }],
            tools: vec![],
            max_output_tokens: Some(32),
        },
        Limits::default(),
        None,
    )
    .map_err(error)?;
    let request = js_sys::Object::new();
    Reflect::set(&request, &"model".into(), &provider.model()?)?;
    Reflect::set(
        &request,
        &"serializedInput".into(),
        &Uint8Array::from(prepared.bytes().as_ref()),
    )?;
    if let Some(signal) = signal {
        Reflect::set(&request, &"signal".into(), signal)?;
    }
    Ok(request.into())
}

fn dispatch() -> Result<JsValue, JsValue> {
    to_js(&json!({"operationId":"00000000-0000-4000-8000-000000000001","step":2}))
}

#[wasm_bindgen_test]
async fn registry_drives_every_adapter_through_fetch_with_accounting_and_public_events()
-> Result<(), JsValue> {
    for (adapter, data) in fixtures() {
        let fixture = model_fixture(data, "complete");
        let provider = provider(adapter, &fixture)?;
        let request = request(&provider, None)?;
        let capacity: Value = from_js(provider.context_capacity(provider.model()?)?)?;
        assert_eq!(capacity, json!({"contextTokens":4096,"outputTokens":512}));
        let count = provider.count_tokens(request.clone())?;
        assert_eq!(
            field(&count, "requestDigest")?
                .dyn_into::<Uint8Array>()?
                .length(),
            32
        );
        let tokens: Vec<u32> = from_js(field(&count, "messageTokens")?)?;
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0] >= 5);
        assert_eq!(field(&fixture, "calls")?.as_f64(), Some(0.0));
        let iterator = provider.generate(request, dispatch()?)?;
        let events: Vec<Value> = from_js(JsFuture::from(collect_model(iterator)).await?)?;
        assert_eq!(events.len(), 4);
        assert_eq!(events[0], json!({"kind":"content","delta":"hé"}));
        assert_eq!(events[1], json!({"kind":"reasoning","delta":"think"}));
        let expected_metadata = if adapter == Adapter::Anthropic {
            json!({"anthropic_thinking":[{"type":"thinking","thinking":"think","signature":""}]})
        } else {
            Value::Null
        };
        let mut expected =
            json!({"kind":"tool_call","callId":"call","name":"echo","arguments":{"value":1}});
        if !expected_metadata.is_null() {
            expected["metadata"] = expected_metadata;
        }
        assert_eq!(events[2], expected);
        assert_eq!(events[3]["kind"], "completed");
        assert!(events[3]["metadata"]["usage"].is_object());
        assert_eq!(field(&fixture, "calls")?.as_f64(), Some(1.0));
        let headers = field(&fixture, "headers")?.dyn_into::<web_sys::Headers>()?;
        assert_eq!(
            headers
                .get("anthropic-dangerous-direct-browser-access")?
                .as_deref(),
            (adapter == Adapter::Anthropic).then_some("true"),
        );
        assert!(
            JsFuture::from(provider.reconcile(JsValue::UNDEFINED))
                .await?
                .is_undefined()
        );
    }
    Ok(())
}

#[wasm_bindgen_test]
async fn abort_and_iterator_return_cancel_pending_fetch_headers() -> Result<(), JsValue> {
    for use_return in [false, true] {
        let fixture = model_fixture("", "pending");
        let provider = provider(Adapter::Chat, &fixture)?;
        let controller = web_sys::AbortController::new()?;
        let iterator =
            provider.generate(request(&provider, Some(&controller.signal()))?, dispatch()?)?;
        JsFuture::from(cancel_model(
            iterator,
            &controller,
            field(&fixture, "ready")?,
            use_return,
        ))
        .await?;
        assert_eq!(field(&fixture, "aborted")?.as_bool(), Some(true));
    }
    Ok(())
}

#[wasm_bindgen_test]
async fn already_aborted_requests_never_dispatch() -> Result<(), JsValue> {
    let fixture = model_fixture("", "pending");
    let provider = provider(Adapter::Chat, &fixture)?;
    let controller = web_sys::AbortController::new()?;
    controller.abort();
    let iterator =
        provider.generate(request(&provider, Some(&controller.signal()))?, dispatch()?)?;
    assert!(JsFuture::from(collect_model(iterator)).await.is_err());
    assert_eq!(field(&fixture, "calls")?.as_f64(), Some(0.0));
    Ok(())
}

#[wasm_bindgen_test]
fn accounting_and_dispatch_reject_tampered_requests_without_effects() -> Result<(), JsValue> {
    let fixture = model_fixture("", "pending");
    let provider = provider(Adapter::Chat, &fixture)?;
    let request = request(&provider, None)?;
    Reflect::set(
        &request,
        &"serializedInput".into(),
        &Uint8Array::from(b"invalid".as_slice()),
    )?;
    assert!(provider.count_tokens(request.clone()).is_err());
    assert!(provider.generate(request, dispatch()?).is_err());
    assert_eq!(field(&fixture, "calls")?.as_f64(), Some(0.0));
    Ok(())
}

#[wasm_bindgen_test]
async fn browser_reader_is_verified_and_projected_by_rust_without_counting_io()
-> Result<(), JsValue> {
    use acyclic_harness::{
        conversation::{FileDescriptor, FileRef},
        model::{FileProjectionPolicy, ModelContentPart},
    };
    let data = "hé\nfile".as_bytes();
    let fixture = content_fixture(&Uint8Array::from(data));
    let mut registry = WasmProviderRegistry::new(
        to_js(&json!({}))?,
        to_js(&Limits::default())?,
        Some(field(&fixture, "fetch")?.dyn_into::<Function>()?),
        Some(field(&fixture, "reader")?.dyn_into::<Function>()?),
    )?;
    registry.register(
        "local".into(),
        to_js(&json!({
            "adapter":"chat","authentication":"optional","base_url":"http://localhost:1234/v1",
            "models":{"fixture":{"context_tokens":4096,"output_tokens":512}},"max_event_bytes":4096,
        }))?,
    )?;
    let provider = registry.resolve("local/fixture", Some(to_js(&json!({"temperature":0.1}))?))?;
    let file = FileRef::new(
        from_js(to_js(&json!({"provider":{"namespace":"test","family":"filesystem","version":"2"},"id":"project","class":"project","owner":{"kind":"project","id":"project"}}))?)?,
        "file.txt".into(), "1".into(), FileDescriptor::from_bytes(data,"text/plain",&mut None).map_err(error)?, "file.txt".into(), &mut None,
    ).map_err(error)?;
    let prepared = PreparedModelRequest::prepare(
        ModelRequest {
            model: provider.provider.model().clone(),
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: ModelContent::Part(ModelContentPart::File {
                    file,
                    policy: FileProjectionPolicy::BoundedFull,
                }),
            }],
            tools: vec![],
            max_output_tokens: Some(32),
        },
        Limits::default(),
        None,
    )
    .map_err(error)?;
    let request = js_sys::Object::new();
    Reflect::set(&request, &"model".into(), &provider.model()?)?;
    Reflect::set(
        &request,
        &"serializedInput".into(),
        &Uint8Array::from(prepared.bytes().as_ref()),
    )?;
    provider.count_tokens(request.clone().into())?;
    assert_eq!(field(&fixture, "reads")?.as_f64(), Some(0.0));
    JsFuture::from(collect_model(
        provider.generate(request.into(), dispatch()?)?,
    ))
    .await?;
    let body: Value = from_js(field(&fixture, "body")?)?;
    assert_eq!(body["messages"][0]["content"][0]["text"], "hé\nfile");
    assert_eq!(body["temperature"], 0.1);
    assert_eq!(field(&fixture, "reads")?.as_f64(), Some(1.0));
    assert_eq!(field(&fixture, "calls")?.as_f64(), Some(1.0));
    Ok(())
}
