import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "bun:test";
import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";
import type { Invoke } from "../../pi/index.ts";

// Pi supplies these modules to installed extensions through its loader. Resolve
// the real pinned TUI here as well; the source shim lives outside this fixture.
const require = createRequire(import.meta.url);
const { createJiti } = require("jiti");
const extension: typeof import("../../pi/index.ts").default = await createJiti(import.meta.url, {
  tryNative: false,
  alias: { "@earendil-works/pi-tui": require.resolve("@earendil-works/pi-tui") },
}).import("../../pi/index.ts", { default: true });

type Handler = (event: Record<string, unknown>, context: ExtensionContext) => Promise<unknown>;
const definitions = ["bash", "fs", "read", "write", "edit"].map(name => ({
  name, description: `Harness ${name}`, input_schema: { type: "object", properties: {} },
}));
function setup(send: Invoke) {
  const handlers = new Map<string, Handler>();
  const tools = new Map<string, ToolDefinition>();
  const api = {
    on(event: string, handler: Handler) { assert.ok(!handlers.has(event)); handlers.set(event, handler); },
    registerTool(tool: ToolDefinition) { tools.set(tool.name, tool); },
  } as unknown as ExtensionAPI;
  extension(api, send);
  const context = {
    cwd: "/selected-project", sessionManager: { getSessionId: () => "native-session" },
  } as unknown as ExtensionContext;
  return { handlers, tools, context };
}

test("Pi executes every registered tool exactly once through the harness", async () => {
  const calls: { event: string; input: Record<string, unknown> }[] = [];
  const result = { content: [{type: "text", text: "harness result"}], details: {published:true}, isError:false };
  const {handlers,tools,context} = setup(async (event,input) => {
    calls.push({event,input});
    return event === "catalog" ? {tools:definitions} : event === "tool_call" ? result : {};
  });
  await handlers.get("session_start")!({},context);
  assert.deepEqual([...tools.keys()],definitions.map(tool=>tool.name));
  for (const [name,tool] of tools) {
    assert.deepEqual(tool.parameters,definitions.find(definition=>definition.name===name)!.input_schema);
    assert.equal(typeof tool.renderCall,"function");
    assert.equal(typeof tool.renderResult,"function");
    const count = calls.length;
    assert.equal(await handlers.get("tool_call")!({toolName:name},context),undefined);
    assert.equal(calls.length,count,"permission hook must not execute a tool");
    const answer = await tool.execute("call",{},undefined,undefined,context);
    assert.deepEqual(answer,{content:result.content,details:result.details});
    assert.deepEqual(calls.at(-1),{event:"tool_call",input:{toolCallId:"call",toolName:name,input:{},sessionId:"native-session",cwd:"/selected-project"}});
    assert.equal(calls.length,count+1);
  }
  assert.deepEqual(await handlers.get("tool_call")!({toolName:"unbound"},context),{block:true,reason:"Use the registered harness tools"});
  await handlers.get("session_shutdown")!({},context);
  assert.equal(calls.at(-1)!.event,"session_shutdown");
});

test("Pi cannot fall back to a built-in after a missing or failed harness result", async () => {
  for (const answer of [{block:true,reason:"rejected"},{content:[],isError:true},{}]) {
    const {handlers,tools,context} = setup(async event => event === "catalog" ? {tools:definitions} : answer);
    await handlers.get("session_start")!({},context);
    await assert.rejects(()=>tools.get("bash")!.execute("call",{},undefined,undefined,context));
  }
});
