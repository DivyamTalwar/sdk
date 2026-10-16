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

function countReads<T>(operation: () => T, key: string): { value: T; reads: () => number; restore: () => void } {
  const original = Object.getOwnPropertyDescriptor;
  let reads = 0;
  Object.getOwnPropertyDescriptor = (value, property) => {
    if (property === key) reads++;
    return original(value, property);
  };
  try { return { value: operation(), reads: () => reads, restore: () => { Object.getOwnPropertyDescriptor = original; } }; }
  catch (error) { Object.getOwnPropertyDescriptor = original; throw error; }
}

function assertLinear(count: number, reads: number): void { expect(reads).toBeLessThanOrEqual(count); }
function appendPages(count: number, pageSize: number, depth: number, measure = retainedArrayTailBytes): number {
  const array: unknown[] = [];
  let bytes = 0;
  for (let start = 0; start < count; start += pageSize) {
    for (let index = start; index < Math.min(start + pageSize, count); index++) {
      let nested: unknown = null;
      for (let level = 0; level < depth; level++) nested = { next: nested };
      array.push({ probe: index, nested });
    }
    bytes = measure(array, Number.MAX_SAFE_INTEGER, start, bytes);
  }
  return bytes;
}

test("incremental array work visits each new record once across arbitrary page cuts", () => {
  fc.assert(fc.property(fc.integer({ min: 0, max: 256 }), fc.integer({ min: 1, max: 64 }),
    fc.integer({ min: 0, max: 16 }), (count, pageSize, depth) => {
      const measured = countReads(() => appendPages(count, pageSize, depth), "probe");
      try { assertLinear(count, measured.reads()); }
      finally { measured.restore(); }
    }), { numRuns: 128 });
});

test("linear work assertion rejects the original complete-prefix scans", () => {
  const measured = countReads(() => appendPages(129, 64, 0, (array, maximum) =>
    legacyRetainedDataBytes(array, maximum)), "probe");
  try { expect(measured.reads()).toBe(64 + 128 + 129); expect(() => assertLinear(129, measured.reads())).toThrow(); }
  finally { measured.restore(); }
});

test("real persisted pages read each command twice and preserve array wrapper capacity", async () => {
  const count = 129;
  const commands = Array.from({ length: count }, (_, index) => pageCommand(index));
  const maximum = legacyRetainedDataBytes(commands, Number.MAX_SAFE_INTEGER);
  for (const limit of [maximum - 1, maximum]) {
    const store = await outboxFixture(count, limit);
    const measured = countReads(() => store.load(), "operationId");
    try {
      if (limit === maximum) expect(await measured.value).toEqual(commands);
      else await expect(measured.value).rejects.toThrow("client command byte capacity exceeded");
      expect(measured.reads()).toBeLessThanOrEqual(2 * count);
    } finally { measured.restore(); }
  }
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
