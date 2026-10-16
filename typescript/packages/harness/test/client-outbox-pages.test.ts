import { expect, test } from "bun:test";
import fc from "fast-check";
import { retainedArrayTailBytes, retainedDataBytes } from "../src/client-data.js";
import { IndexedDbClientStore } from "../src/client-store.js";
import { legacyRetainedDataBytes } from "./client-data-byte-oracle.js";
import { outboxFixture, pageCommand } from "./client-outbox-page-fixture.js";

type Outcome = { bytes: number } | { error: string };
function outcome(operation: () => number): Outcome {
  try { return { bytes: operation() }; }
  catch (error) { return { error: `${(error as Error).name}: ${(error as Error).message}` }; }
}

test("immutable array prefixes preserve every byte limit and failure boundary", () => {
  fc.assert(fc.property(
    fc.array(fc.jsonValue(), { maxLength: 64 }),
    fc.array(fc.integer({ min: 0, max: 16 }), { maxLength: 12 }),
    fc.integer({ min: 1, max: 100_000 }), fc.boolean(),
    (values, cuts, maximum, boundary) => {
      if (boundary) maximum = Math.max(1, legacyRetainedDataBytes(values, Number.MAX_SAFE_INTEGER) - 1);
      const array: unknown[] = [];
      let bytes = 0, cursor = 0;
      for (const size of [...cuts, values.length]) {
        const start = array.length;
        array.push(...values.slice(cursor, cursor + size));
        cursor = array.length;
        const expected = outcome(() => legacyRetainedDataBytes(array, maximum));
        expect(outcome(() => retainedDataBytes(array, maximum))).toEqual(expected);
        const actual = outcome(() => retainedArrayTailBytes(array, maximum, start, bytes));
        expect(actual).toEqual(expected);
        if ("error" in actual) return;
        bytes = actual.bytes;
      }
    },
  ), { numRuns: 256 });
});

test("appended values retain depth, cycle, accessor, binary and budget error precedence", () => {
  for (const value of [undefined, { entry: undefined }, [undefined]]) for (const allowed of [false, true]) {
    expect(outcome(() => retainedDataBytes(value, 100_000, allowed)))
      .toEqual(outcome(() => legacyRetainedDataBytes(value, 100_000, allowed)));
  }
  const accessor = Object.defineProperty({}, "entry", { enumerable: true, get() { throw new Error("getter ran"); } });
  const cycle: { self?: unknown } = {};
  cycle.self = cycle;
  const values: unknown[] = [undefined, () => {}, new Map(), accessor, cycle, new Uint8Array(new ArrayBuffer(64), 8, 1), 1n];
  for (const depth of [62, 63, 64, 65]) {
    let value: unknown = null;
    for (let index = 0; index < depth; index++) value = { next: value };
    values.push(value);
  }
  for (const value of values) for (const maximum of [32, 96, 200, 100_000]) {
    const array = [null];
    const prefix = legacyRetainedDataBytes(array, 100_000);
    if (prefix > maximum) continue;
    const appended: unknown[] = [...array, value];
    expect(outcome(() => retainedArrayTailBytes(appended, maximum, 1, prefix)))
      .toEqual(outcome(() => legacyRetainedDataBytes(appended, maximum)));
  }
  const array: unknown[] = [null];
  const prefix = legacyRetainedDataBytes(array, 100_000);
  array.push(array);
  expect(outcome(() => retainedArrayTailBytes(array, 100_000, 1, prefix)))
    .toEqual(outcome(() => legacyRetainedDataBytes(array, 100_000)));
});

function countReads<T>(operation: () => T): { value: T; reads: () => number; restore: () => void } {
  const original = BigInt.prototype.toString;
  let reads = 0;
  // A primitive observation cannot retain a command or any of its children.
  BigInt.prototype.toString = function (this: bigint, radix?: number) {
    reads++;
    return original.call(this, radix);
  };
  try { return { value: operation(), reads: () => reads, restore: () => { BigInt.prototype.toString = original; } }; }
  catch (error) { BigInt.prototype.toString = original; throw error; }
}

test("real persisted pages read each command twice and preserve array wrapper capacity", async () => {
  const count = 129;
  const commands = Array.from({ length: count }, (_, index) => pageCommand(index));
  const maximum = legacyRetainedDataBytes(commands, Number.MAX_SAFE_INTEGER);
  for (const limit of [maximum - 1, maximum]) {
    const store = await outboxFixture(count, limit);
    const measured = countReads(() => store.load());
    try {
      if (limit === maximum) expect(await measured.value).toEqual(commands);
      else await expect(measured.value).rejects.toThrow("client command byte capacity exceeded");
      expect(measured.reads()).toBeLessThanOrEqual(2 * count);
    } finally { measured.restore(); }
  }
});

test("descriptor instrumentation cannot retain private page clones between reads", async () => {
  const maximum = 200_000;
  const store = await outboxFixture(129, maximum);
  const original = Object.getOwnPropertyDescriptor;
  const candidates: { kind: string }[] = [];
  Object.getOwnPropertyDescriptor = (value, key) => {
    const descriptor = original(value, key);
    if (key === "operationId") {
      if (descriptor?.value === pageCommand(0).operationId) candidates.push(value as { kind: string });
      if (descriptor?.value === pageCommand(64).operationId && candidates.length > 1) {
        candidates[1]!.kind = "x".repeat(maximum);
      }
    }
    return descriptor;
  };
  try {
    const commands = await store.load();
    expect(legacyRetainedDataBytes(commands, maximum)).toBeLessThanOrEqual(maximum);
    expect(commands).toEqual(Array.from({ length: 129 }, (_, index) => pageCommand(index)));
  } finally { Object.getOwnPropertyDescriptor = original; }
});

test("clone instrumentation cannot expose an earlier page while cloning the next", async () => {
  const maximum = 200_000;
  const store = await outboxFixture(129, maximum);
  const original = Object.defineProperty;
  let escaped: { kind: string } | undefined;
  Object.defineProperty = (value, key, descriptor) => {
    const result = original(value, key, descriptor);
    if (key === "offlineSafe" && descriptor.value === true) {
      const command = value as { operationId: string; kind: string };
      if (command.operationId === pageCommand(0).operationId) escaped = command;
      if (command.operationId === pageCommand(64).operationId && escaped) escaped.kind = "x".repeat(maximum);
    }
    return result;
  };
  try {
    const commands = await store.load();
    expect(legacyRetainedDataBytes(commands, maximum)).toBeLessThanOrEqual(maximum);
    expect(commands).toEqual(Array.from({ length: 129 }, (_, index) => pageCommand(index)));
  } finally { Object.defineProperty = original; }
});

test("foreign pages keep full revalidation after the built-in reader is restored", async () => {
  const store = await outboxFixture(65, 100_000);
  const first = pageCommand(0);
  store.loadPage = async () => {
    store.loadPage = IndexedDbClientStore.prototype.loadPage;
    // The outer load consumes this page before mutation, then awaits IDB.
    queueMicrotask(() => queueMicrotask(() => { (first as { kind: string }).kind = "x".repeat(100_000); }));
    return { commands: [first], afterSequence: 1, throughSequence: 65 };
  };
  await expect(store.load()).rejects.toThrow("client command byte capacity exceeded");
});

test("replacing the prototype reader does not make foreign pages private", async () => {
  const original = IndexedDbClientStore.prototype.loadPage;
  const store = new IndexedDbClientStore({ databaseName: "foreign-prototype", maximumResidentBytes: 100_000 });
  const first = pageCommand(0);
  IndexedDbClientStore.prototype.loadPage = async after => {
    if (!after) return { commands: [first], afterSequence: 1, throughSequence: 2 };
    (first as { kind: string }).kind = "x".repeat(100_000);
    return { commands: [pageCommand(1)], afterSequence: 2, throughSequence: 2 };
  };
  try { await expect(store.load()).rejects.toThrow("client command byte capacity exceeded"); }
  finally { IndexedDbClientStore.prototype.loadPage = original; }
});
