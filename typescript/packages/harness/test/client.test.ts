import { beforeAll, expect, test } from "bun:test";
import "fake-indexeddb/auto";
import { HarnessClient, ClientQuery, TaskVolume, WasmStream, initializeClientQueries } from "../src/client.js";

beforeAll(initializeClientQueries);
const encode = new TextEncoder();
const limits = { commands: 8n, bytes: 4096n, durability: "Accepted" } as const;
const queryLimits = { files: 32n, segments: 64n, bytes: 8192n };
const request = (id: string) => ({ Task: { task: "task", file: { Request: id } } }) as const;

async function setup(durable = false) {
  const name = `file-client-${crypto.randomUUID()}`;
  const stream = durable ? await WasmStream.openBrowser(name, 64, 65536n) : new WasmStream();
  const outbox = await TaskVolume.create(stream, "client/files");
  const target = await TaskVolume.create(stream, "agent/files");
  const policy = { ...limits, durability: durable ? "Durable" : "Accepted" } as const;
  const client = await HarnessClient.open(outbox, target, policy);
  return { name, stream, outbox, target, client, policy,
    free() { client.free(); outbox.free(); target.free(); stream.free(); } };
}

test("the exported client is the Rust file client and restores enqueue order", async () => {
  const f = await setup();
  await f.client.enqueue(request("z.json"), encode.encode("first"));
  await f.client.enqueue(request("a.json"), encode.encode("second"));
  expect(f.client.pending()).toBe(2);
  const query = await ClientQuery.open(f.target, "Tasks", null, queryLimits);
  const stop = Promise.withResolvers<void>();
  const seen: string[] = [];
  const follow = query.follow(delivery => { if (delivery.file) seen.push(new TextDecoder().decode(delivery.file.bytes)); }, () => {
    if (seen.length === 2) stop.resolve();
  }, stop.promise);
  f.client.free();
  const restored = await HarnessClient.open(f.outbox, f.target, f.policy);
  expect(restored.pending()).toBe(2);
  expect(await restored.flush()).toBe(2);
  await follow;
  expect(seen).toEqual(["first", "second"]);
  expect(await restored.flush()).toBe(0);
  query.free(); restored.free(); f.outbox.free(); f.target.free(); f.stream.free();
});

test("duplicate requests keep identity and conflicting metadata cannot replace them", async () => {
  const f = await setup();
  await f.client.enqueue(request("one.json"), encode.encode("one"));
  await f.client.enqueue(request("one.json"), encode.encode("one"));
  expect(f.client.pending()).toBe(1);
  await expect(f.client.enqueue(request("one.json"), encode.encode("different"))).rejects.toBeDefined();
  expect(f.client.pending()).toBe(1);
  expect(await f.client.flush()).toBe(1);
  await f.client.enqueue(request("one.json"), encode.encode("one"));
  expect(await f.client.flush()).toBe(0);
  f.free();
});

test("request and byte bounds reject before storing another file", async () => {
  const f = await setup();
  f.client.free();
  const client = await HarnessClient.open(f.outbox, f.target, { ...f.policy, commands: 1n, bytes: 3n });
  await client.enqueue(request("one.json"), encode.encode("one"));
  const before = await f.outbox.head();
  await expect(client.enqueue(request("two.json"), encode.encode("two"))).rejects.toBeDefined();
  await expect(client.enqueue("Goal", encode.encode("goal"))).rejects.toBeDefined();
  expect(await f.outbox.head()).toEqual(before);
  expect(client.pending()).toBe(1);
  client.free(); f.outbox.free(); f.target.free(); f.stream.free();
});

test("a stale client cannot move the persisted cursor backwards", async () => {
  const f = await setup();
  await f.client.enqueue(request("one.json"), encode.encode("one"));
  await f.client.enqueue(request("two.json"), encode.encode("two"));
  const stale = await HarnessClient.open(f.outbox, f.target, f.policy);
  expect(await f.client.flush()).toBe(2);
  const cursor = f.client.cursor();
  await expect(stale.flush()).rejects.toBeDefined();
  stale.free();
  const reopened = await HarnessClient.open(f.outbox, f.target, f.policy);
  expect(reopened.cursor()).toEqual(cursor);
  expect(reopened.pending()).toBe(0);
  reopened.free(); f.free();
});

test("persistent provider files recover pending intent after all handles close", async () => {
  const f = await setup(true);
  await f.client.enqueue(request("offline.json"), encode.encode("queued"));
  const { name, policy } = f;
  f.free();
  const stream = await WasmStream.openBrowser(name, 64, 65536n);
  const outbox = await TaskVolume.open(stream, "client/files");
  const target = await TaskVolume.open(stream, "agent/files");
  const client = await HarnessClient.open(outbox, target, policy);
  expect(client.pending()).toBe(1);
  expect(await client.flush()).toBe(1);
  const query = await ClientQuery.open(target, "Tasks", null, queryLimits);
  const views = query.views();
  expect(new TextDecoder().decode(views.segments(request("offline.json"))![0])).toBe("queued");
  views.free(); query.free(); client.free(); outbox.free(); target.free(); stream.free();
});
