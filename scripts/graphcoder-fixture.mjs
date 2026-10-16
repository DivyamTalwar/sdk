// Production adapter over a loopback scripted HTTP endpoint; no model in the binary.
import { createServer } from "node:http";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export async function fixture() {
  const root = mkdtempSync(join(tmpdir(), "graphcoder-"));
  let fail = false;
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const bytes of request) body += bytes;
    const input = JSON.parse(body);
    if (!Array.isArray(input.messages)) { response.writeHead(400); response.end(); return; }
    if (fail) { response.writeHead(400); response.end("scripted provider error"); return; }
    response.writeHead(200, { "content-type": "text/event-stream" });
    for (const text of ["graphcoder ", "**test answer**"]) {
      response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: text }, finish_reason: null }] })}\n\n`);
    }
    response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 2 } })}\n\ndata: [DONE]\n\n`);
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const catalog = join(root, "catalog.json");
  writeFileSync(catalog, JSON.stringify({ scripted: {
    adapter: "chat", authentication: "optional", base_url: `http://127.0.0.1:${server.address().port}/v1`,
    models: { test: { context_tokens: 8192, output_tokens: 1024 } }, max_event_bytes: 65536,
  } }));
  return {
    root, args: ["--workspace", root, "--model", "scripted/test", "--catalog", catalog],
    fail: () => { fail = true; },
    close: async () => { await new Promise(resolve => server.close(resolve)); rmSync(root, { recursive: true, force: true }); },
  };
}
