import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "@sinclair/typebox";
import { spawn } from "node:child_process";

// Installation substitutes one JSON string; no shell interprets it.
const executable = "__ACYCLIC_EXECUTABLE__";
const maximum = 64 * 1024;
type Answer = Record<string, unknown>;
export type Invoke = (event: string, input: Answer) => Promise<Answer>;

/** The common Rust client owns normalization, authentication, append and follow. */
export const invoke: Invoke = (event, input) => new Promise((resolve, reject) => {
  const child = spawn(executable, event === "catalog" ? ["__catalog"] : ["__hook", "pi", event], { windowsHide: true, stdio: "pipe" });
  const output: Buffer[] = [];
  let bytes = 0;
  child.on("error", reject);
  child.stdin.on("error", reject);
  child.stdout.on("data", (chunk: Buffer) => {
    bytes += chunk.length;
    if (bytes > maximum) { child.kill(); reject(new Error("Harness hook answer exceeds its bound")); }
    else output.push(chunk);
  });
  // Diagnostics go to the native host; task output comes only from followed files.
  child.stderr.on("data", (chunk: Buffer) => process.stderr.write(chunk));
  child.on("close", code => {
    try {
      const answer: unknown = JSON.parse(Buffer.concat(output).toString("utf8"));
      if (!answer || typeof answer !== "object" || Array.isArray(answer)) throw new Error("Invalid harness hook answer");
      if (code !== 0 && !("block" in answer && answer.block === true)) throw new Error("Harness hook failed");
      resolve(answer as Answer);
    } catch (error) { reject(error); }
  });
  const payload = JSON.stringify(input);
  if (Buffer.byteLength(payload) > maximum) { child.kill(); reject(new Error("Native hook exceeds its bound")); }
  else child.stdin.end(payload);
});

function identity(ctx: ExtensionContext) {
  return { sessionId: ctx.sessionManager.getSessionId(), cwd: ctx.cwd };
}

/** Tool definitions and execution both come from the harness registry. */
export default function extension(pi: ExtensionAPI, send: Invoke = invoke) {
  const registered = new Set<string>();
  pi.on("session_start", async (_event, ctx) => {
    await send("session_start", identity(ctx));
    const catalog = await send("catalog", {});
    if (!Array.isArray(catalog.tools)) throw new Error("Invalid harness tool catalog");
    for (const tool of catalog.tools) {
      if (!tool || typeof tool.name !== "string" || typeof tool.description !== "string" ||
          !tool.input_schema || typeof tool.input_schema !== "object") throw new Error("Invalid harness tool definition");
      pi.registerTool({
        name: tool.name, label: tool.name, description: tool.description,
        parameters: Type.Unsafe(tool.input_schema),
        async execute(toolCallId, input, _signal, _onUpdate, context) {
          const result = await send("tool_call", { toolCallId, toolName: tool.name, input, ...identity(context) });
          if (!Array.isArray(result.content) || result.isError !== false) throw new Error("Invalid harness tool result");
          return { content: result.content, details: result.details };
        },
      });
      registered.add(tool.name);
    }
  });
  pi.on("tool_call", async event => registered.has(event.toolName)
    ? undefined : { block: true, reason: "Use the registered harness tools" });
  pi.on("tool_result", async (event, ctx) => send("tool_result", { ...event, ...identity(ctx) }));
  pi.on("session_shutdown", async (_event, ctx) => { await send("session_shutdown", identity(ctx)); });
}
