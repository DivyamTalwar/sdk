import { expect, test } from "bun:test";
import {
  DEFAULT_LIMITS, ModelRegistry, initializeModelProviders,
  type ModelEvent, type ModelProvider, type ModelRequest,
} from "../src/index.js";
import { prepareModelRequest } from "../generated/wasm/acyclic_harness_wasm.js";

test("Rust model registry implements the public TS provider contract", async () => {
  await initializeModelProviders();
  let submitted: Record<string, unknown> | undefined;
  let headers: Headers | undefined;
  const registry = new ModelRegistry({}, DEFAULT_LIMITS, async (_url, init) => {
    headers = new Headers(init?.headers);
    if (!(init?.body instanceof Uint8Array)) throw new Error("Rust fetch body must be bytes");
    submitted = JSON.parse(new TextDecoder().decode(init.body)) as Record<string, unknown>;
    return new Response([
      'data: {"choices":[{"delta":{"content":"hi"}}]}',
      'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"echo","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}',
      'data: {"choices":[],"usage":{"total_tokens":3}}',
      "data: [DONE]",
    ].join("\n\n") + "\n\n");
  });
  registry.register("ollama", {
    headers: { "x-owner": "project" },
    adapter: "chat", authentication: "optional", base_url: "http://localhost:11434/v1", max_event_bytes: 4096,
    models: { fixture: { context_tokens: 4096, output_tokens: 512 } },
  });
  const selected = registry.resolve("ollama/fixture", { temperature: 0.1 });
  const provider: ModelProvider = selected;
  const model = selected.model;
  const request: ModelRequest = { model, messages: [{ role: "user", content: "hello" }], tools: [] };
  const admitted = { ...request, serializedInput: prepareModelRequest(request, DEFAULT_LIMITS) };
  expect(provider.contextCapacity(model)).toEqual({ contextTokens: 4096, outputTokens: 512 });
  const count = provider.countTokens(admitted);
  expect(count.requestDigest).toBeInstanceOf(Uint8Array);
  expect(count.requestDigest.length).toBe(32);
  expect(count.messageTokens.length).toBe(1);
  expect(submitted).toBeUndefined();
  const events: ModelEvent[] = [];
  const dispatch = { operationId: "00000000-0000-4000-8000-000000000001", step: 0 };
  for await (const event of provider.generate(admitted, dispatch)) events.push(event);
  expect(events).toEqual([
    { kind: "content", delta: "hi" },
    { kind: "tool_call", callId: "call", name: "echo", arguments: {} },
    { kind: "completed", metadata: { finish_reason: "tool_calls", usage: { total_tokens: 3 } } },
  ]);
  expect(submitted?.stream).toBe(true);
  expect(submitted?.temperature).toBe(0.1);
  expect(headers?.get("x-owner")).toBe("project");
  expect(await provider.reconcile({ ...dispatch, requestDigest: count.requestDigest, observed: [] })).toBeUndefined();
  selected.free();
  registry.free();
});
