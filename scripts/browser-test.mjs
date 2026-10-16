// Headless Chromium qualification over its pushed CDP pipe; no polling.
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";

export async function browserTest(module, args) {
  const root = resolve(import.meta.dirname, "..");
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/") return new Response("<!doctype html><title>Browser qualification</title>", { headers: { "content-type": "text/html" } });
    const file = resolve(root, `.${decodeURIComponent(path)}`);
    if (!file.startsWith(`${root}${sep}`)) return new Response(null, { status: 404 });
    return new Response(Bun.file(file));
  } });
  const profile = await mkdtemp(join(tmpdir(), "acyclic-browser-test-"));
  const chrome = spawn(process.env.CHROME ?? "/usr/bin/chromium", ["--headless=new", "--no-sandbox", "--remote-debugging-pipe", "--no-first-run", `--user-data-dir=${profile}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe", "pipe", "pipe"], windowsHide: true });
  const exited = new Promise(resolve => chrome.once("exit", resolve));
  const pending = new Map(), events = new Map();
  let sequence = 0, buffer = "", errors = "";
  chrome.stderr.on("data", data => { errors += data; });
  const fail = error => { for (const { reject } of pending.values()) reject(error); pending.clear(); };
  chrome.once("error", fail);
  chrome.once("exit", code => fail(new Error(`Chromium exited ${code}: ${errors}`)));
  chrome.stdio[4].on("data", data => {
    buffer += data;
    for (;;) {
      const end = buffer.indexOf("\0");
      if (end < 0) break;
      const message = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
      if (message.id) {
        const result = pending.get(message.id); pending.delete(message.id);
        if (message.error) result?.reject(new Error(JSON.stringify(message.error)));
        else result?.resolve(message.result);
      } else {
        const key = `${message.sessionId ?? ""}:${message.method}`;
        events.get(key)?.(message.params); events.delete(key);
      }
    }
  });
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = ++sequence; pending.set(id, { resolve, reject });
    chrome.stdio[3].write(`${JSON.stringify({ id, method, params, sessionId })}\0`);
  });
  const deadline = setTimeout(() => { fail(new Error("browser qualification exceeded 9 seconds")); chrome.kill(); }, 9_000);
  try {
    const { targetId } = await send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
    await send("Page.enable", {}, sessionId);
    const loaded = new Promise(resolve => events.set(`${sessionId}:Page.loadEventFired`, resolve));
    await send("Page.navigate", { url: server.url.href }, sessionId); await loaded;
    const expression = `import(${JSON.stringify(`/${module}`)}).then(module => module.test(${JSON.stringify(args)}))`;
    const result = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? JSON.stringify(result.exceptionDetails));
    return result.result.value;
  } finally {
    clearTimeout(deadline);
    if (chrome.exitCode === null) chrome.kill();
    await exited; server.stop(true);
    await rm(profile, { recursive: true, force: true }).catch(error => console.error(`profile cleanup: ${error.message}`));
  }
}
