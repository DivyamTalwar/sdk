import assert from "node:assert/strict";
import { test } from "bun:test";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import extension, { type Invoke } from "../../pi/index.ts";

type Handler = (event: Record<string, unknown>, context: ExtensionContext) => Promise<unknown>;

function setup(send: Invoke) {
  const handlers = new Map<string, Handler>();
  const api = { on(event: string, handler: Handler) {
    assert.ok(!handlers.has(event));
    handlers.set(event, handler);
  } } as unknown as ExtensionAPI;
  extension(api, send);
  const context = {
    cwd: "/selected-project",
    sessionManager: { getSessionId: () => "native-session" },
  } as unknown as ExtensionContext;
  return { handlers, context };
}

test("Pi forwards native events and identities through the common client", async () => {
  const calls: { event: string; input: Record<string, unknown> }[] = [];
  const { handlers, context } = setup(async (event, input) => {
    calls.push({ event, input });
    return event === "tool_call" ? { block: false } : {};
  });
  const scenarios = [
    ["session_start", {}],
    ["tool_call", { toolCallId: "call", toolName: "bash", input: { command: "pwd" } }],
    ["tool_result", { toolCallId: "call", isError: false, content: [{ type: "text", text: "/project" }] }],
    ["session_shutdown", {}],
  ] as const;
  assert.deepEqual([...handlers.keys()], scenarios.map(([event]) => event));
  for (const [event, input] of scenarios) {
    const handler = handlers.get(event);
    assert.ok(handler);
    await handler({ ...input, sessionId: "untrusted-session", cwd: "/untrusted" }, context);
    assert.deepEqual(calls.at(-1), {
      event, input: { ...input, sessionId: "native-session", cwd: "/selected-project" },
    });
  }
});

test("Pi applies only the returned tool input and blocks failed authorization", async () => {
  for (const failure of [false, true]) {
    const { handlers, context } = setup(async () => {
      if (failure) throw new Error("authorization unavailable");
      return { block: false, updatedInput: { command: "approved command" } };
    });
    const handler = handlers.get("tool_call");
    assert.ok(handler);
    const input = { command: "original command", obsolete: "remove" };
    const result = await handler({ toolCallId: "call", toolName: "bash", input }, context);
    if (failure) {
      assert.deepEqual(result, { block: true, reason: "Error: authorization unavailable" });
      assert.deepEqual(input, { command: "original command", obsolete: "remove" });
    } else {
      assert.deepEqual(input, { command: "approved command" });
      assert.deepEqual(result, { block: false, updatedInput: input });
    }
  }
});
