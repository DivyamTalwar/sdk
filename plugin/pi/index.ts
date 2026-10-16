import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { spawn } from "node:child_process";

// Installation substitutes one JSON string; no shell interprets it.
const executable = "__ACYCLIC_EXECUTABLE__";
const maximum = 64 * 1024;
type Answer = Record<string, unknown>;
export type Invoke = (event: string, input: Answer) => Promise<Answer>;

/** The common Rust client owns normalization, authentication, append and follow. */
export const invoke: Invoke = (event, input) => new Promise((resolve, reject) => {
  const child = spawn(executable, ["__hook", "pi", event], { windowsHide: true, stdio: "pipe" });
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

/** Pi owns native execution; the shim owns no task queue, process or transcript. */
export default function extension(pi: ExtensionAPI, send: Invoke = invoke) {
  pi.on("session_start", async (_event, ctx) => { await send("session_start", identity(ctx)); });
  pi.on("tool_call", async (event, ctx) => {
    try {
      const answer = await send("tool_call", { ...event, ...identity(ctx) });
      if (answer.updatedInput && typeof answer.updatedInput === "object" && !Array.isArray(answer.updatedInput)) {
        for (const key of Object.keys(event.input)) delete event.input[key];
        Object.assign(event.input, answer.updatedInput);
      }
      return answer;
    } catch (error) { return { block: true, reason: String(error) }; }
  });
  pi.on("tool_result", async (event, ctx) => send("tool_result", { ...event, ...identity(ctx) }));
  pi.on("session_shutdown", async (_event, ctx) => { await send("session_shutdown", identity(ctx)); });
}
