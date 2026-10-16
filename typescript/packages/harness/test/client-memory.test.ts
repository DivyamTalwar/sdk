import { beforeAll, expect, test } from "bun:test";
import initialize from "../generated/wasm/acyclic_harness_wasm.js";
import { HarnessClient, TaskVolume, WasmStream, initializeClientQueries } from "../src/client.js";

beforeAll(initializeClientQueries);

function bounded(before: number, after: number) {
  if (after !== before) throw new Error("oversize admission grew WASM memory");
}

test("oversize enqueue checks its JS buffer length without growing WASM memory", async () => {
  const stream = new WasmStream();
  const outbox = await TaskVolume.create(stream, "client/files");
  const target = await TaskVolume.create(stream, "agent/files");
  const client = await HarnessClient.open(outbox, target, { commands: 1n, bytes: 3n, durability: "Accepted" });
  const { memory } = await initialize();
  const payload = new Uint8Array(Math.max(16 * 1024 * 1024, memory.buffer.byteLength + 65536));
  const key = { Task: { task: "task", file: { Request: "large.json" } } } as const;
  const before = memory.buffer.byteLength;
  for (let retry = 0; retry < 8; retry++) {
    await expect(client.enqueue(key, payload)).rejects.toEqual({ kind: "ByteQuota", detail: { limit: 3n } });
    bounded(before, memory.buffer.byteLength);
  }
  expect(client.pending()).toBe(0);
  // The former Vec<u8> boundary still exists on an unbounded volume write.
  // The same checker must detect its eager copy, independently of its result.
  const controlBefore = memory.buffer.byteLength;
  await target.write(await target.head(), "Goal", payload, null).catch(() => {});
  expect(() => bounded(controlBefore, memory.buffer.byteLength)).toThrow("oversize admission grew WASM memory");
  client.free(); outbox.free(); target.free(); stream.free();
});
