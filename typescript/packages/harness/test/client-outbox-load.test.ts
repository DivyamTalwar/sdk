import { expect, test } from "bun:test";
import fc from "fast-check";
import { type ClientCommand } from "../src/client.js";
import { cloneStructuredValue } from "../src/client-data.js";
import { MemoryOutbox } from "../src/client-store.js";

const authority = { kind: "conversation" as const, id: "outbox-test" };
const command = (index: number, sequence: number): ClientCommand => ({
  operationId: `01010101-0101-0101-0101-${String(index).padStart(12, "0")}` as ClientCommand["operationId"],
  authority, kind: "message.append", payload: { metadata: { sequence } }, offlineSafe: true,
});
function metadata(value: ClientCommand): { sequence: number } {
  if (value.offlineSafe !== true) throw new Error("unexpected unsafe command");
  return value.payload.metadata as { sequence: number };
}

function countSets<T>(operation: () => T): { value: T; sets: number } {
  const original = globalThis.Set;
  let sets = 0;
  globalThis.Set = new Proxy(original, {
    construct(target, args, newTarget) {
      sets++;
      return Reflect.construct(target, args, newTarget);
    },
  });
  try { return { value: operation(), sets }; }
  finally { globalThis.Set = original; }
}

function assertCloneStorage(count: number, sets: number): void {
  expect(sets).toBe(count === 0 ? 0 : 1);
}

test("outbox load preserves ordered owned snapshots under caller and reader mutation", async () => {
  await fc.assert(fc.asyncProperty(
    fc.array(fc.integer(), { maxLength: 24 }),
    async sequences => {
      const outbox = new MemoryOutbox();
      const expected = sequences.map((sequence, index) => command(index, sequence));
      for (const value of expected) {
        const input = structuredClone(value);
        await outbox.put(input);
        metadata(input).sequence++;
      }
      const measured = countSets(() => outbox.load());
      assertCloneStorage(expected.length, measured.sets);
      const loaded = await measured.value;
      expect(loaded).toEqual(expected);
      for (let index = 0; index < loaded.length; index++) {
        expect(loaded[index]).not.toBe(expected[index]);
        expect(metadata(loaded[index]!)).not.toBe(metadata(expected[index]!));
        metadata(loaded[index]!).sequence++;
      }
      expect(await outbox.load()).toEqual(expected);
    },
  ), { numRuns: 128 });
});

type Outcome = { value: unknown } | { error: string };
function outcome(operation: () => unknown): Outcome {
  try { return { value: operation() }; }
  catch (error) { return { error: `${(error as Error).name}: ${(error as Error).message}` }; }
}

function verifyReusedAncestors(values: readonly unknown[], copy = cloneStructuredValue): void {
  const ancestors = new Set<object>();
  for (const value of values) {
    const expected = outcome(() => cloneStructuredValue(value, new Set()));
    const actual = outcome(() => copy(value, ancestors));
    expect(actual).toEqual(expected);
    expect(ancestors.size).toBe(0);
  }
}

test("reusing ancestors equals fresh sets and retains nothing after successful or failed clones", () => {
  fc.assert(fc.property(fc.array(fc.jsonValue(), { maxLength: 12 }), values => {
    verifyReusedAncestors(values);
  }), { numRuns: 256 });
  const cycle: { self?: unknown } = {};
  cycle.self = cycle;
  const accessor = Object.defineProperty({}, "entry", { enumerable: true, get() { throw new Error("getter ran"); } });
  const shared = { label: "shared" };
  verifyReusedAncestors([
    { nested: cycle }, { nested: accessor }, { nested: new Map() },
    { nested: () => {} }, [shared, shared], shared, { nested: undefined },
    new Uint8Array([1, 2]), new ArrayBuffer(8), { nested: 1n },
  ]);
});

test("ancestor property rejects a clone that retains a completed root", () => {
  const value = { nested: { sequence: 1 } };
  expect(() => verifyReusedAncestors([value, value], (input, ancestors) => {
    const result = cloneStructuredValue(input, ancestors);
    ancestors.add(input as object);
    return result;
  })).toThrow();
});

test("clone storage bound rejects the original per-command sets", () => {
  const values = Array.from({ length: 64 }, (_, index) => ({ command: command(index, index) }));
  const measured = countSets(() => [...values.values()]
    .map(({ command: value }) => cloneStructuredValue(value, new Set())));
  expect(measured.value).toEqual(values.map(value => value.command));
  expect(measured.sets).toBe(values.length);
  expect(() => assertCloneStorage(values.length, measured.sets)).toThrow();
});
