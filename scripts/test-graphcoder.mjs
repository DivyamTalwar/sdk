#!/usr/bin/env node
// Installed CLI contract: the client emits only canonical wire JSONL on stdout.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { isAbsolute } from "node:path";
import { fixture } from "./graphcoder-fixture.mjs";
const [binary] = process.argv.slice(2);
assert.ok(binary && isAbsolute(binary), "absolute installed binary required");
const deadline = AbortSignal.timeout(8000);
const host = await fixture();
async function run(args, input = "") {
  const child = spawn(binary, args, { windowsHide: true, stdio: ["pipe", "pipe", "pipe"], signal: deadline, killSignal: "SIGKILL" });
  let stdout = "", stderr = "";
  child.stdout.on("data", bytes => { stdout += bytes; });
  child.stderr.on("data", bytes => { stderr += bytes; });
  child.stdin.end(input);
  const code = await new Promise((resolve, reject) => { child.on("error", reject); child.on("exit", (code, signal) => signal ? reject(new Error(`CLI terminated by ${signal}`)) : resolve(code)); });
  return { code, stdout, stderr };
}
try {
  const help = await run(["--help"]);
  assert.equal(help.code, 0);
  for (const option of ["--prompt", "--input-format", "--workspace", "--model"]) assert.ok(help.stdout.includes(option), option);
  for (const removed of ["--fixture", "--session", "--prepare-scenario"]) assert.notEqual((await run([removed, "unused"])).code, 0);
  const prompt = await run([...host.args, "-p", "say hello"]);
  assert.equal(prompt.code, 0, prompt.stderr);
  const frames = prompt.stdout.trim().split("\n").map(line => JSON.parse(line));
  assert.ok(frames.some(frame => frame.handshake), "client handshake missing");
  assert.ok(frames.some(frame => frame.admission), "client admission missing");
  assert.ok(frames.some(frame => frame.delivery), "client replay missing");
  assert.ok(frames.some(frame => frame.status), "client settlement missing");
  assert.ok(frames.every(frame => !Object.keys(frame).some(key => key.includes("scenario"))));
  const malformed = await run([...host.args, "--input-format", "jsonl", "--operation", "00000000-0000-0000-0000-000000000001"], "not JSON\n");
  assert.notEqual(malformed.code, 0, "malformed wire stdin must fail");
  host.fail();
  assert.notEqual((await run([...host.args, "-p", "provider failure"])).code, 0);
} finally { await host.close(); }
