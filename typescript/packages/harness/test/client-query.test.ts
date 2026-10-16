import { beforeAll, expect, test } from "bun:test";
import { ClientQuery, WasmStream, TaskVolume, initializeClientQueries } from "../src/client-query.js";

beforeAll(initializeClientQueries);

const limits = { files: 32n, segments: 64n, bytes: 4096n };
const encoder = new TextEncoder();
const decoder = new TextDecoder();
function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function chunk(turn: bigint, sequence: bigint) {
  return { Task: { task: "t", file: { Chunk: { turn, sequence } } } } as const;
}
function text(turn: bigint) {
  return { Task: { task: "t", file: { TextHead: { turn, channel: "Text" } } } } as const;
}
async function fixture() {
  const stream = new WasmStream();
  const volume = await TaskVolume.create(stream, "agent/files");
  const query = await ClientQuery.open(volume, "Root", null, limits);
  const views = query.views();
  async function write(turn: bigint, sequence: bigint, value: string) {
    return volume.write(await volume.head(), chunk(turn, sequence), encoder.encode(value), "Text");
  }
  return { stream, volume, query, views, write, free() { views.free(); query.free(); volume.free(); stream.free(); } };
}

test("live getters and per-chunk output share the native file fold", async () => {
  const f = await fixture();
  const cut = f.views.cursor();
  const first = deferred();
  const release = deferred();
  const stop = deferred();
  const deliveries: unknown[] = [];
  const running = f.query.follow(async delivery => {
    deliveries.push(delivery);
    expect(delivery.file?.bytes).toBeInstanceOf(Uint8Array);
    expect(decoder.decode(delivery.file!.bytes)).toBe("one");
    expect(f.views.cursor()).toEqual(cut);
    expect(f.views.files()).toEqual([]);
    first.resolve();
    await release.promise;
  }, () => {
    expect(f.views.cursor().position).toBe(cut.position + 1n);
    expect(f.views.segments(text(2n))!.map(segment => decoder.decode(segment))).toEqual(["one"]);
    stop.resolve();
  }, stop.promise);
  expect(f.views.cursor()).toEqual(cut);
  await f.write(2n, 1n, "one");
  await first.promise;
  expect(f.views.cursor()).toEqual(cut);
  expect(f.views.files()).toEqual([]);
  release.resolve();
  await running;
  expect(deliveries).toHaveLength(1);
  expect(await f.query.reconnect()).toBe(false);
  f.free();
});

test("failed publication retains the frame across later arrivals and replay", async () => {
  const f = await fixture();
  const cut = f.views.cursor();
  const never = new Promise<void>(() => {});
  await f.write(2n, 1n, "current");
  let attempts = 0;
  const failed = f.query.follow(() => { attempts++; throw new Error("output rejected"); }, () => {}, never);
  try { await failed; throw new Error("expected rejection"); }
  catch (error) { expect(String(error)).toContain("output rejected"); }
  expect(attempts).toBe(1);
  expect(f.views.cursor()).toEqual(cut);
  expect(f.views.files()).toEqual([]);
  await expect(f.query.reconnect()).rejects.toContain("buffered");
  await f.write(1n, 1n, "older");
  const stop = deferred();
  const output: string[] = [];
  await f.query.follow(delivery => { output.push(decoder.decode(delivery.file!.bytes)); }, () => {
    if (output.length === 2) stop.resolve();
  }, stop.promise);
  expect(output).toEqual(["current", "older"]);
  expect(f.views.segments(text(2n))!.map(segment => decoder.decode(segment))).toEqual(["current"]);
  expect(f.views.segments(text(1n))!.map(segment => decoder.decode(segment))).toEqual(["older"]);
  const reopened = await ClientQuery.open(f.volume, "Root", f.views.cursor(), limits);
  const replay = reopened.views();
  expect(replay.cursor()).toEqual(f.views.cursor());
  expect(replay.files()).toEqual(f.views.files());
  for (const turn of [1n, 2n]) expect(replay.segments(text(turn))).toEqual(f.views.segments(text(turn)));
  replay.free(); reopened.free(); f.free();
});

test("cancellation drains an in-flight publication and concurrent follows fail", async () => {
  const f = await fixture();
  const cut = f.views.cursor();
  const started = deferred();
  const release = deferred();
  const cancel = deferred();
  let notifications = 0;
  const running = f.query.follow(async () => { started.resolve(); await release.promise; }, () => { notifications++; }, cancel.promise);
  await f.write(1n, 1n, "accepted");
  await started.promise;
  await expect(f.query.follow(() => {}, () => {}, cancel.promise)).rejects.toContain("already following");
  await expect(f.query.reconnect()).rejects.toContain("already following");
  cancel.resolve();
  expect(f.views.cursor()).toEqual(cut);
  release.resolve();
  await running;
  expect(notifications).toBe(1);
  expect(f.views.cursor().position).toBe(cut.position + 1n);
  expect(f.views.segments(text(1n))!.map(segment => decoder.decode(segment))).toEqual(["accepted"]);
  // Cancellation while waiting leaves the shared handle readable and resumable.
  const idle = deferred();
  const waiting = f.query.follow(() => { throw new Error("unexpected data"); }, () => {}, idle.promise);
  idle.resolve();
  await waiting;
  expect(await f.query.reconnect()).toBe(false);
  f.free();
});

test("opening a missing volume is read-only and writes use canonical validation", async () => {
  const stream = new WasmStream();
  await expect(TaskVolume.open(stream, "missing/files")).rejects.toBeDefined();
  const volume = await TaskVolume.create(stream, "agent/files");
  const before = await volume.head();
  await expect(volume.write(before, { Input: "workspace/../secret" }, encoder.encode("bad"), null)).rejects.toBeDefined();
  await expect(volume.write(before, chunk(1n, 1n), encoder.encode("missing channel"), null)).rejects.toBeDefined();
  expect(await volume.head()).toEqual(before);
  volume.free(); stream.free();
});
