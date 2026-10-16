import { estimateShallowMemoryUsageOf, gcAndSweep, heapStats } from "bun:jsc";
import { type ClientCommand } from "../src/client.js";

const { MemoryOutbox: Before } = await import(process.env.HARNESS_OUTBOX_BEFORE ??
  new URL("../src/client-store.ts", import.meta.url).href) as typeof import("../src/client-store.js");
const { MemoryOutbox: After } = await import("../src/client-store.js");
const mode = process.env.HARNESS_OUTBOX_CASE ?? "timing";
let checksum = 0;
const median = (values: number[]) => values.sort((left, right) => left - right)[values.length >> 1]!;

for (const count of [0, 1, 64, 1024]) {
  const stores = [mode.endsWith("after") ? undefined : new Before(),
    mode.endsWith("before") ? undefined : new After()];
  const commands: ClientCommand[] = Array.from({ length: count }, (_, index) => ({
    operationId: `01010101-0101-0101-0101-${String(index).padStart(12, "0")}` as ClientCommand["operationId"],
    authority: { kind: "conversation", id: "outbox-bench" }, kind: "message.append",
    payload: { metadata: { sequence: index, pending: true, ordinal: BigInt(index) } }, offlineSafe: true,
  }));
  for (const store of stores) if (store) for (const command of commands) await store.put(command);
  const run = async (index: number, iterations: number): Promise<number> => {
    const start = performance.now();
    for (let iteration = 0; iteration < iterations; iteration++) {
      const loaded = await stores[index]!.load();
      if (loaded.length !== count) throw new Error("load count changed");
      checksum += loaded.length;
    }
    return (performance.now() - start) * 1_000_000 / iterations;
  };
  if (mode === "timing") {
    const iterations = count < 2 ? 1000 : count === 64 ? 16 : 2;
    for (let round = 0; round < 8; round++) {
      await run(0, iterations); await run(1, iterations);
    }
    const before: number[] = [], after: number[] = [], paired: number[] = [], controls: number[] = [];
    for (let pair = 0; pair < 8; pair++) {
      const times = [0, 0];
      for (const index of pair % 2 === 0 ? [0, 1] : [1, 0]) times[index] = await run(index, iterations);
      before.push(times[0]!); after.push(times[1]!);
      paired.push(100 * (1 - times[1]! / times[0]!));
      const control = await run(0, iterations);
      controls.push(100 * (1 - (await run(0, iterations)) / control));
    }
    console.log(JSON.stringify({ count, beforeNs: median(before), afterNs: median(after),
      pairedPercent: median(paired), wins: paired.filter(gain => gain > 0).length,
      range: [Math.min(...paired), Math.max(...paired)], controlPercent: median(controls),
      controlRange: [Math.min(...controls), Math.max(...controls)] }));
  } else {
    for (let index = 0; index < 2; index++) {
      if (!stores[index]) continue;
      await run(index, 4);
      gcAndSweep();
      if (mode.startsWith("syscalls")) {
        process.stderr.write("OUTBOX_BEGIN\n");
        await run(index, 1);
        process.stderr.write("OUTBOX_END\n");
        continue;
      }
      const before = heapStats().objectTypeCounts;
      const censusResult = stores[index]!.load();
      const after = heapStats().objectTypeCounts;
      checksum += (await censusResult).length;
      gcAndSweep();
      const original = globalThis.Set;
      let sets = 0, estimatedSetBytes = 0;
      globalThis.Set = new Proxy(original, { construct(target, args, newTarget) {
        const value = Reflect.construct(target, args, newTarget);
        sets++;
        estimatedSetBytes += estimateShallowMemoryUsageOf(value);
        return value;
      } });
      let result: Promise<readonly ClientCommand[]>;
      try { result = stores[index]!.load(); }
      finally { globalThis.Set = original; }
      const loaded = await result;
      checksum += loaded.length;
      console.log(JSON.stringify({ count, legacy: index === 0, sets, estimatedSetBytes,
        uninstrumentedSetCensus: (after.Set ?? 0) - (before.Set ?? 0),
        uninstrumentedArrayCensus: (after.Array ?? 0) - (before.Array ?? 0),
        processRss: process.memoryUsage().rss }));
    }
  }
}
console.log(JSON.stringify({ checksum, mode }));
