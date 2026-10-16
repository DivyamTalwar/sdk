#!/usr/bin/env node
// Real PTY over the production registry/client/TUI path. Fail if any binding is missing.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { fixture } from "./graphcoder-fixture.mjs";
const [binary] = process.argv.slice(2);
assert.ok(binary && isAbsolute(binary), "absolute installed binary required");
const host = await fixture();
const quote = text => `'${text.replaceAll("'", "'\\''")}'`;
let launcher, args;
if (process.platform === "win32") {
  launcher = join(process.env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe");
  args = ["-NoProfile", "-NonInteractive", "-File", fileURLToPath(new URL("graphcoder-conpty.ps1", import.meta.url)), "-Binary", binary, "-Workspace", host.root, "-SettingsRoot", host.settings, "-Catalog", host.args.at(-1), "-Model", "scripted/test", "-DeadlineMilliseconds", "8000"];
} else {
  launcher = "/usr/bin/script";
  const command = [binary, ...host.args].map(quote).join(" ");
  args = process.platform === "linux" ? ["-q", "-e", "-f", "-c", command, "/dev/null"] : ["-q", "/dev/null", "/bin/sh", "-c", command];
}
const child = spawn(launcher, args, { windowsHide: true, env: { ...process.env, TERM: "xterm-256color" }, stdio: ["pipe", "pipe", "pipe"] });
let output = "", submitted = false, completed = false;
const timeout = setTimeout(() => child.kill("SIGKILL"), 8000);
child.stdout.on("data", bytes => {
  output += bytes.toString("utf8");
  const visible = output.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
  if (!submitted && visible.includes("GraphCoder")) { submitted = true; child.stdin.write("say hello\r"); }
  if (!completed && visible.includes("test answer")) { completed = true; child.stdin.write("\x04"); }
});
child.stderr.on("data", bytes => { output += bytes.toString("utf8"); });
try {
  const code = await new Promise((resolve, reject) => { child.on("error", reject); child.on("exit", (code, signal) => signal ? reject(new Error(`PTY terminated by ${signal}: ${output}`)) : resolve(code)); });
  assert.equal(code, 0, output);
  assert.ok(submitted, "composed prompt never appeared");
  assert.ok(completed, "streamed Markdown never reached the real PTY");
  assert.ok(output.includes("\x1b["), "no terminal frames emitted");
} finally { clearTimeout(timeout); child.kill(); await host.close(); }
